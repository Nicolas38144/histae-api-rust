use super::{
    BillingReconciliationStore, CustomerCreationContext, CustomerRecoveryResult,
    ReconciliationApplyResult, ReconciliationApplyState, ReconciliationCursor, ReconciliationKind,
    ReconciliationRow, ReconciliationStoreError, ReconciliationStoreFuture, SubscriptionContext,
};
use crate::billing::{
    domain::StripeSubscriptionStatus,
    webhook::{SubscriptionProjection, status_name},
};
use crate::infra::postgres::{ConstraintKind, Database, DatabaseError, map_sqlx_error};
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
#[derive(Clone)]
pub struct PgBillingReconciliationStore {
    database: Database,
}

impl PgBillingReconciliationStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn apply_subscription_transaction(
        &self,
        context: SubscriptionContext,
        projection: Option<SubscriptionProjection>,
        snapshot_at: DateTime<Utc>,
        next_due_at: DateTime<Utc>,
        customer_deleted: bool,
    ) -> Result<ReconciliationApplyResult, ReconciliationStoreError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let mapping: Option<String> = sqlx::query_scalar(
                        "SELECT customer.stripe_customer_id
                         FROM billing_customer AS customer
                         JOIN user_account AS account ON account.user_id = customer.user_id
                         WHERE customer.user_id = $1
                           AND customer.stripe_customer_deleted_at IS NULL
                           AND account.deleted_at IS NULL
                         FOR UPDATE OF customer",
                    )
                    .bind(context.user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if mapping.as_deref() != Some(context.stripe_customer_id.as_str()) {
                        return Ok(ReconciliationApplyResult {
                            state: ReconciliationApplyState::NotFound,
                            previous_status: None,
                            status: None,
                        });
                    }
                    let current = sqlx::query(
                        "SELECT provider, status, projection_version, provider_snapshot_at
                         FROM user_subscription WHERE user_id = $1 FOR UPDATE",
                    )
                    .bind(context.user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let current_version = current
                        .as_ref()
                        .map(|row| row.try_get::<i64, _>("projection_version"))
                        .transpose()
                        .map_err(map_sqlx_error)?;
                    let current_status = current
                        .as_ref()
                        .map(|row| row.try_get::<Option<String>, _>("status"))
                        .transpose()
                        .map_err(map_sqlx_error)?
                        .flatten()
                        .as_deref()
                        .and_then(StripeSubscriptionStatus::parse);
                    let provider_snapshot_at = current
                        .as_ref()
                        .map(|row| row.try_get::<Option<DateTime<Utc>>, _>("provider_snapshot_at"))
                        .transpose()
                        .map_err(map_sqlx_error)?
                        .flatten();
                    if current_version != context.projection_version
                        || provider_snapshot_at.is_some_and(|current| current > snapshot_at)
                    {
                        return Ok(ReconciliationApplyResult {
                            state: ReconciliationApplyState::Stale,
                            previous_status: current_status,
                            status: current_status,
                        });
                    }
                    let mut next_status = current_status;
                    if let Some(projection) = projection {
                        next_status = Some(projection.status);
                        sqlx::query(
                            "INSERT INTO user_subscription (
                               user_id, plan, provider, provider_subscription_id, provider_price_id,
                               billing_period, status, cancel_at_period_end, current_period_starts_at,
                               current_period_ends_at, trial_ends_at, canceled_at, projection_version,
                               provider_snapshot_at, updated_at
                             ) VALUES ($1, 'premium', 'stripe', $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $11, clock_timestamp())
                             ON CONFLICT (user_id) DO UPDATE SET
                               plan = 'premium', provider = 'stripe',
                               provider_subscription_id = EXCLUDED.provider_subscription_id,
                               provider_price_id = EXCLUDED.provider_price_id,
                               billing_period = EXCLUDED.billing_period, status = EXCLUDED.status,
                               cancel_at_period_end = EXCLUDED.cancel_at_period_end,
                               current_period_starts_at = EXCLUDED.current_period_starts_at,
                               current_period_ends_at = EXCLUDED.current_period_ends_at,
                               trial_ends_at = EXCLUDED.trial_ends_at, canceled_at = EXCLUDED.canceled_at,
                               projection_version = user_subscription.projection_version + 1,
                               provider_snapshot_at = EXCLUDED.provider_snapshot_at,
                               updated_at = clock_timestamp()",
                        )
                        .bind(context.user_id)
                        .bind(&projection.stripe_subscription_id)
                        .bind(&projection.stripe_price_id)
                        .bind(projection.billing_period.as_str())
                        .bind(status_name(projection.status))
                        .bind(projection.cancel_at_period_end)
                        .bind(projection.current_period_starts_at)
                        .bind(projection.current_period_ends_at)
                        .bind(projection.trial_ends_at)
                        .bind(projection.canceled_at)
                        .bind(snapshot_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if let Some(trial_started_at) = projection.trial_starts_at {
                            sqlx::query(
                                "UPDATE billing_customer
                                 SET trial_used_at = COALESCE(trial_used_at, $2)
                                 WHERE user_id = $1",
                            )
                            .bind(context.user_id)
                            .bind(trial_started_at)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }
                    } else if current.as_ref().is_some_and(|row| {
                        row.try_get::<Option<String>, _>("provider")
                            .ok()
                            .flatten()
                            .as_deref()
                            == Some("stripe")
                    }) {
                        next_status = Some(StripeSubscriptionStatus::Canceled);
                        sqlx::query(
                            "UPDATE user_subscription
                             SET status = 'canceled', cancel_at_period_end = false,
                               canceled_at = COALESCE(canceled_at, $2),
                               projection_version = projection_version + 1,
                               provider_snapshot_at = $2, updated_at = clock_timestamp()
                             WHERE user_id = $1",
                        )
                        .bind(context.user_id)
                        .bind(snapshot_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    }
                    sqlx::query(
                        "UPDATE billing_customer
                         SET stripe_reconciled_at = $2, stripe_reconciliation_due_at = $3,
                           stripe_customer_deleted_at = CASE WHEN $4
                             THEN COALESCE(stripe_customer_deleted_at, $2)
                             ELSE stripe_customer_deleted_at END,
                           updated_at = clock_timestamp()
                         WHERE user_id = $1",
                    )
                    .bind(context.user_id)
                    .bind(snapshot_at)
                    .bind(next_due_at)
                    .bind(customer_deleted)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(ReconciliationApplyResult {
                        state: ReconciliationApplyState::Applied,
                        previous_status: current_status,
                        status: next_status,
                    })
                })
            })
            .await
    }
}

impl BillingReconciliationStore for PgBillingReconciliationStore {
    fn schedule_due(&self, now: DateTime<Utc>, limit: u16) -> ReconciliationStoreFuture<'_, u32> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let customers = sqlx::query(
                            "WITH candidates AS MATERIALIZED (
                               SELECT checkout.id, checkout.customer_creation_started_at,
                                 checkout.created_customer_id
                               FROM billing_checkout_session AS checkout
                               JOIN user_account AS account ON account.user_id = checkout.user_id
                               LEFT JOIN billing_customer AS mapping ON mapping.user_id = checkout.user_id
                                 AND mapping.stripe_customer_deleted_at IS NULL
                               LEFT JOIN outbox_event AS queued ON queued.event_type = 'billing.customer.reconcile'
                                 AND queued.aggregate_id = checkout.id
                               WHERE checkout.customer_creation_started_at IS NOT NULL
                                 AND checkout.customer_erased_at IS NULL AND account.deleted_at IS NULL
                                 AND ((checkout.created_customer_id IS NULL
                                     AND checkout.customer_creation_started_at <= $1 - interval '23 hours')
                                   OR (checkout.created_customer_id IS NOT NULL
                                     AND mapping.stripe_customer_id IS DISTINCT FROM checkout.created_customer_id))
                                 AND (queued.id IS NULL OR queued.status IN ('completed', 'discarded'))
                               ORDER BY checkout.customer_creation_started_at, checkout.id
                               FOR UPDATE OF checkout SKIP LOCKED LIMIT $2
                             )
                             INSERT INTO outbox_event (id, event_type, aggregate_id, available_at)
                             SELECT uuid_generate_v4(), 'billing.customer.reconcile', id,
                               CASE WHEN created_customer_id IS NULL
                                 THEN GREATEST(clock_timestamp(), customer_creation_started_at + interval '23 hours')
                                 ELSE clock_timestamp() END
                             FROM candidates
                             ON CONFLICT (event_type, aggregate_id) DO UPDATE SET
                               status = 'pending', attempts = 0, available_at = EXCLUDED.available_at,
                               locked_at = NULL, locked_by = NULL, last_error_code = NULL,
                               processed_at = NULL, dead_lettered_at = NULL,
                               resolved_at = NULL, resolved_by = NULL, resolution_reason = NULL
                             WHERE outbox_event.status IN ('completed', 'discarded')
                             RETURNING id",
                        )
                        .bind(now)
                        .bind(i64::from(limit))
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let customer_count = u32::try_from(customers.len())
                            .map_err(|_| ReconciliationStoreError::InvalidStoredData)?;
                        let remaining = u32::from(limit).saturating_sub(customer_count);
                        if remaining == 0 {
                            return Ok(customer_count);
                        }
                        let subscriptions = sqlx::query(
                            "WITH candidates AS MATERIALIZED (
                               SELECT customer.user_id
                               FROM billing_customer AS customer
                               JOIN user_account AS account ON account.user_id = customer.user_id
                               LEFT JOIN outbox_event AS queued ON queued.event_type = 'billing.subscription.reconcile'
                                 AND queued.aggregate_id = customer.user_id
                               WHERE customer.stripe_customer_deleted_at IS NULL
                                 AND customer.stripe_reconciliation_due_at <= $1
                                 AND account.deleted_at IS NULL
                                 AND (queued.id IS NULL OR queued.status IN ('completed', 'discarded'))
                               ORDER BY customer.stripe_reconciliation_due_at, customer.user_id
                               FOR UPDATE OF customer SKIP LOCKED LIMIT $2
                             )
                             INSERT INTO outbox_event (id, event_type, aggregate_id)
                             SELECT uuid_generate_v4(), 'billing.subscription.reconcile', user_id FROM candidates
                             ON CONFLICT (event_type, aggregate_id) DO UPDATE SET
                               status = 'pending', attempts = 0, available_at = clock_timestamp(),
                               locked_at = NULL, locked_by = NULL, last_error_code = NULL,
                               processed_at = NULL, dead_lettered_at = NULL,
                               resolved_at = NULL, resolved_by = NULL, resolution_reason = NULL
                             WHERE outbox_event.status IN ('completed', 'discarded')
                             RETURNING id",
                        )
                        .bind(now)
                        .bind(i64::from(remaining))
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let subscription_count = u32::try_from(subscriptions.len())
                            .map_err(|_| ReconciliationStoreError::InvalidStoredData)?;
                        Ok(customer_count + subscription_count)
                    })
                })
                .await
        })
    }

    fn subscription_context(
        &self,
        user_id: Uuid,
    ) -> ReconciliationStoreFuture<'_, Option<SubscriptionContext>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                "SELECT customer.user_id, customer.stripe_customer_id,
                        subscription.projection_version
                 FROM billing_customer AS customer
                 JOIN user_account AS account ON account.user_id = customer.user_id
                 LEFT JOIN user_subscription AS subscription ON subscription.user_id = customer.user_id
                 WHERE customer.user_id = $1
                   AND customer.stripe_customer_deleted_at IS NULL
                   AND account.deleted_at IS NULL",
            )
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                Ok(SubscriptionContext {
                    user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                    stripe_customer_id: row
                        .try_get("stripe_customer_id")
                        .map_err(map_sqlx_error)?,
                    projection_version: row
                        .try_get("projection_version")
                        .map_err(map_sqlx_error)?,
                })
            })
            .transpose()
        })
    }

    fn customer_creation_context(
        &self,
        attempt_id: Uuid,
    ) -> ReconciliationStoreFuture<'_, Option<CustomerCreationContext>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                "SELECT checkout.id, checkout.user_id, checkout.customer_creation_started_at,
                        checkout.created_customer_id, checkout.customer_erased_at,
                        customer.stripe_customer_id AS mapped_customer_id
                 FROM billing_checkout_session AS checkout
                 JOIN user_account AS account ON account.user_id = checkout.user_id
                 LEFT JOIN billing_customer AS customer ON customer.user_id = checkout.user_id
                   AND customer.stripe_customer_deleted_at IS NULL
                 WHERE checkout.id = $1 AND checkout.customer_creation_started_at IS NOT NULL
                   AND account.deleted_at IS NULL",
            )
            .bind(attempt_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                Ok(CustomerCreationContext {
                    attempt_id: row.try_get("id").map_err(map_sqlx_error)?,
                    user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                    started_at: row
                        .try_get("customer_creation_started_at")
                        .map_err(map_sqlx_error)?,
                    created_customer_id: row
                        .try_get("created_customer_id")
                        .map_err(map_sqlx_error)?,
                    mapped_customer_id: row
                        .try_get("mapped_customer_id")
                        .map_err(map_sqlx_error)?,
                    customer_erased_at: row
                        .try_get("customer_erased_at")
                        .map_err(map_sqlx_error)?,
                })
            })
            .transpose()
        })
    }

    fn apply_subscription(
        &self,
        context: SubscriptionContext,
        projection: Option<SubscriptionProjection>,
        snapshot_at: DateTime<Utc>,
        next_due_at: DateTime<Utc>,
        customer_deleted: bool,
    ) -> ReconciliationStoreFuture<'_, ReconciliationApplyResult> {
        Box::pin(async move {
            match self
                .apply_subscription_transaction(
                    context,
                    projection,
                    snapshot_at,
                    next_due_at,
                    customer_deleted,
                )
                .await
            {
                Err(ReconciliationStoreError::Database(DatabaseError::Constraint(
                    ConstraintKind::Unique,
                ))) => Ok(ReconciliationApplyResult {
                    state: ReconciliationApplyState::Conflict,
                    previous_status: None,
                    status: None,
                }),
                result => result,
            }
        })
    }

    fn recover_customer_creation(
        &self,
        attempt_id: Uuid,
        customer_id: Option<String>,
    ) -> ReconciliationStoreFuture<'_, CustomerRecoveryResult> {
        Box::pin(async move {
            let result = self
                .database
                .transaction(|connection| {
                    Box::pin(async move {
                        let checkout = sqlx::query(
                            "SELECT checkout.user_id, checkout.created_customer_id,
                                    checkout.customer_erased_at
                             FROM billing_checkout_session AS checkout
                             JOIN user_account AS account ON account.user_id = checkout.user_id
                             WHERE checkout.id = $1 AND account.deleted_at IS NULL
                             FOR UPDATE OF checkout",
                        )
                        .bind(attempt_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some(checkout) = checkout else {
                            return Ok(CustomerRecoveryResult::NotFound);
                        };
                        let user_id: Uuid = checkout.try_get("user_id").map_err(map_sqlx_error)?;
                        let created: Option<String> = checkout
                            .try_get("created_customer_id")
                            .map_err(map_sqlx_error)?;
                        let erased: Option<DateTime<Utc>> = checkout
                            .try_get("customer_erased_at")
                            .map_err(map_sqlx_error)?;
                        if erased.is_some() {
                            return Ok(CustomerRecoveryResult::AlreadyResolved);
                        }
                        if created.is_some() && customer_id.is_some() && created != customer_id {
                            return Ok(CustomerRecoveryResult::Conflict);
                        }
                        let recovered = created.or(customer_id);
                        let Some(recovered) = recovered else {
                            sqlx::query(
                                "UPDATE billing_checkout_session
                                 SET customer_creation_started_at = NULL, status = 'failed',
                                   checkout_url = NULL, updated_at = clock_timestamp()
                                 WHERE id = $1",
                            )
                            .bind(attempt_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            return Ok(CustomerRecoveryResult::Cleared);
                        };
                        let mapped: Option<Uuid> = sqlx::query_scalar(
                            "INSERT INTO billing_customer
                               (user_id, stripe_customer_id, stripe_reconciliation_due_at)
                             VALUES ($1, $2, clock_timestamp())
                             ON CONFLICT (user_id) DO UPDATE SET
                               stripe_customer_id = EXCLUDED.stripe_customer_id,
                               stripe_customer_deleted_at = NULL,
                               stripe_reconciliation_due_at = clock_timestamp(),
                               updated_at = clock_timestamp()
                             WHERE billing_customer.stripe_customer_id = EXCLUDED.stripe_customer_id
                               OR billing_customer.stripe_customer_deleted_at IS NOT NULL
                             RETURNING user_id",
                        )
                        .bind(user_id)
                        .bind(&recovered)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if mapped.is_none() {
                            return Ok(CustomerRecoveryResult::Conflict);
                        }
                        sqlx::query(
                            "UPDATE billing_checkout_session
                             SET created_customer_id = $2, status = 'failed', checkout_url = NULL,
                               updated_at = clock_timestamp() WHERE id = $1",
                        )
                        .bind(attempt_id)
                        .bind(recovered)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(CustomerRecoveryResult::Recovered)
                    })
                })
                .await;
            match result {
                Err(ReconciliationStoreError::Database(DatabaseError::Constraint(
                    ConstraintKind::Unique,
                ))) => Ok(CustomerRecoveryResult::Conflict),
                result => result,
            }
        })
    }

    fn list(
        &self,
        kind: Option<ReconciliationKind>,
        limit: u32,
        cursor: Option<ReconciliationCursor>,
    ) -> ReconciliationStoreFuture<'_, Vec<ReconciliationRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let rows = sqlx::query(
                "SELECT event.id,
                   CASE WHEN event.event_type = 'billing.subscription.reconcile'
                     THEN event.aggregate_id ELSE checkout.user_id END AS user_id,
                   CASE WHEN event.event_type = 'billing.subscription.reconcile'
                     THEN 'subscription' ELSE 'customer_creation' END AS kind,
                   event.attempts, event.last_error_code, event.created_at,
                   event.dead_lettered_at
                 FROM outbox_event AS event
                 LEFT JOIN billing_checkout_session AS checkout
                   ON event.event_type = 'billing.customer.reconcile'
                   AND checkout.id = event.aggregate_id
                 WHERE event.event_type IN ('billing.subscription.reconcile', 'billing.customer.reconcile')
                   AND event.status = 'dead_letter'
                   AND (event.event_type = 'billing.subscription.reconcile' OR checkout.user_id IS NOT NULL)
                   AND ($1::text IS NULL
                     OR ($1 = 'subscription' AND event.event_type = 'billing.subscription.reconcile')
                     OR ($1 = 'customer_creation' AND event.event_type = 'billing.customer.reconcile'))
                   AND ($3::timestamptz IS NULL
                     OR (event.dead_lettered_at, event.id) < ($3, $4::uuid))
                 ORDER BY event.dead_lettered_at DESC, event.id DESC LIMIT $2",
            )
            .bind(kind.map(ReconciliationKind::as_str))
            .bind(i64::from(limit))
            .bind(cursor.map(|cursor| cursor.at))
            .bind(cursor.map(|cursor| cursor.id))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|row| {
                    let kind: String = row.try_get("kind").map_err(map_sqlx_error)?;
                    let attempts: i16 = row.try_get("attempts").map_err(map_sqlx_error)?;
                    Ok(ReconciliationRow {
                        event_id: row.try_get("id").map_err(map_sqlx_error)?,
                        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                        kind: match kind.as_str() {
                            "subscription" => ReconciliationKind::Subscription,
                            "customer_creation" => ReconciliationKind::CustomerCreation,
                            _ => return Err(ReconciliationStoreError::InvalidStoredData),
                        },
                        attempts: u16::try_from(attempts)
                            .map_err(|_| ReconciliationStoreError::InvalidStoredData)?,
                        last_error_code: row.try_get("last_error_code").map_err(map_sqlx_error)?,
                        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
                        dead_lettered_at: row
                            .try_get("dead_lettered_at")
                            .map_err(map_sqlx_error)?,
                    })
                })
                .collect()
        })
    }
}
