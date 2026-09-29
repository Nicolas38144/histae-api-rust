use std::fmt;
use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::domain::{
    BeginCheckoutResult, BillingPeriod, CUSTOMER_CREATE_SAFETY_HOURS, CheckoutContext,
    CustomerCreation, PersistedCheckoutSession, StripeSubscriptionStatus, SubscriptionProvider,
    SubscriptionRow,
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

pub type BillingStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BillingStoreError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeginCheckoutInput {
    pub user_id: Uuid,
    pub idempotency_key: Uuid,
    pub billing_period: BillingPeriod,
    pub attempt_id: Uuid,
    pub now: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub stale_before: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingStoreError {
    Database(DatabaseError),
    InvalidStoredData,
    CheckoutAttemptMissing,
    CheckoutCustomerConflict,
}

impl From<DatabaseError> for BillingStoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl fmt::Display for BillingStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Database(error) => error.safe_code(),
            Self::InvalidStoredData => "invalid_billing_data",
            Self::CheckoutAttemptMissing => "checkout_attempt_missing",
            Self::CheckoutCustomerConflict => "checkout_customer_conflict",
        })
    }
}

impl std::error::Error for BillingStoreError {}

pub trait BillingStore: Send + Sync {
    fn subscription_for_user(
        &self,
        user_id: Uuid,
    ) -> BillingStoreFuture<'_, Option<SubscriptionRow>>;

    fn customer_for_user(&self, user_id: Uuid) -> BillingStoreFuture<'_, Option<String>>;

    fn begin_checkout(
        &self,
        input: BeginCheckoutInput,
    ) -> BillingStoreFuture<'_, BeginCheckoutResult>;

    fn begin_customer_creation(&self, attempt_id: Uuid)
    -> BillingStoreFuture<'_, CustomerCreation>;

    fn record_created_customer(
        &self,
        attempt_id: Uuid,
        customer_id: String,
    ) -> BillingStoreFuture<'_, ()>;

    fn save_customer(&self, user_id: Uuid, customer_id: String) -> BillingStoreFuture<'_, bool>;

    fn mark_checkout_open(
        &self,
        attempt_id: Uuid,
        session: PersistedCheckoutSession,
    ) -> BillingStoreFuture<'_, bool>;

    fn mark_checkout_failed(&self, attempt_id: Uuid) -> BillingStoreFuture<'_, ()>;

    fn customer_creations_for_erasure(
        &self,
        user_id: Uuid,
    ) -> BillingStoreFuture<'_, Vec<CustomerCreation>>;

    fn mark_created_customer_erased(&self, attempt_id: Uuid) -> BillingStoreFuture<'_, ()>;
}

#[derive(Clone)]
pub struct PgBillingRepository {
    database: Database,
}

impl PgBillingRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn begin_checkout_transaction(
        &self,
        input: BeginCheckoutInput,
    ) -> Result<BeginCheckoutResult, BillingStoreError> {
        let BeginCheckoutInput {
            user_id,
            idempotency_key,
            billing_period,
            attempt_id,
            now,
            expires_at,
            stale_before,
        } = input;
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let account: Option<Uuid> = sqlx::query_scalar(
                        "SELECT user_id FROM user_account
                         WHERE user_id = $1 AND deleted_at IS NULL FOR UPDATE",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if account.is_none() {
                        return Ok(BeginCheckoutResult::NotFound);
                    }

                    let previous = sqlx::query(
                        "SELECT id, billing_period, stripe_session_id, checkout_url,
                                status, expires_at, updated_at
                         FROM billing_checkout_session
                         WHERE user_id = $1 AND idempotency_key = $2",
                    )
                    .bind(user_id)
                    .bind(idempotency_key)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let previous_id = previous
                        .as_ref()
                        .map(|row| row.try_get::<Uuid, _>("id"))
                        .transpose()
                        .map_err(map_sqlx_error)?;
                    if let Some(row) = &previous {
                        let stored_period: String =
                            row.try_get("billing_period").map_err(map_sqlx_error)?;
                        if BillingPeriod::parse(&stored_period) != Some(billing_period) {
                            return Ok(BeginCheckoutResult::IdempotencyConflict);
                        }
                        let status: String = row.try_get("status").map_err(map_sqlx_error)?;
                        let previous_expiry: DateTime<Utc> =
                            row.try_get("expires_at").map_err(map_sqlx_error)?;
                        if status == "open" && previous_expiry > now {
                            let session_id: Option<String> =
                                row.try_get("stripe_session_id").map_err(map_sqlx_error)?;
                            let url: Option<String> =
                                row.try_get("checkout_url").map_err(map_sqlx_error)?;
                            if let (Some(session_id), Some(url)) = (session_id, url) {
                                return Ok(BeginCheckoutResult::Replay(PersistedCheckoutSession {
                                    session_id,
                                    url,
                                    expires_at: previous_expiry,
                                }));
                            }
                        }
                        if matches!(status.as_str(), "completed" | "expired") {
                            return Ok(BeginCheckoutResult::IdempotencyConsumed);
                        }
                        let updated_at: DateTime<Utc> =
                            row.try_get("updated_at").map_err(map_sqlx_error)?;
                        if status == "creating" && updated_at > stale_before {
                            return Ok(BeginCheckoutResult::InProgress);
                        }
                    }

                    sqlx::query(
                        "UPDATE billing_checkout_session
                         SET status = 'expired', checkout_url = NULL,
                             updated_at = clock_timestamp()
                         WHERE user_id = $1 AND status = 'open' AND expires_at <= $2",
                    )
                    .bind(user_id)
                    .bind(now)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    sqlx::query(
                        "UPDATE billing_checkout_session
                         SET status = 'failed', updated_at = clock_timestamp()
                         WHERE user_id = $1 AND status = 'creating' AND updated_at <= $2",
                    )
                    .bind(user_id)
                    .bind(stale_before)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;

                    if has_active_subscription(connection, user_id, now).await? {
                        return Ok(BeginCheckoutResult::AlreadySubscribed);
                    }
                    let live: Option<i32> = sqlx::query_scalar(
                        "SELECT 1 FROM billing_checkout_session
                         WHERE user_id = $1 AND status IN ('creating', 'open') LIMIT 1",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if live.is_some() {
                        return Ok(BeginCheckoutResult::InProgress);
                    }

                    let unresolved: Option<i32> = sqlx::query_scalar(
                        "SELECT 1 FROM billing_checkout_session AS checkout
                         WHERE checkout.user_id = $1
                           AND checkout.customer_creation_started_at IS NOT NULL
                           AND checkout.customer_erased_at IS NULL
                           AND ($2::uuid IS NULL OR checkout.id <> $2)
                           AND NOT EXISTS (
                             SELECT 1 FROM billing_customer AS customer
                             WHERE customer.user_id = checkout.user_id
                               AND customer.stripe_customer_deleted_at IS NULL
                           )
                         LIMIT 1",
                    )
                    .bind(user_id)
                    .bind(previous_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if unresolved.is_some() {
                        return Ok(BeginCheckoutResult::CustomerReconciliationRequired);
                    }

                    let context = checkout_context(connection, user_id, previous_id).await?;
                    let Some(mut context) = context else {
                        return Err(BillingStoreError::InvalidStoredData);
                    };
                    if let Some(previous_id) = previous_id {
                        sqlx::query(
                            "UPDATE billing_checkout_session
                             SET status = 'creating', stripe_session_id = NULL,
                                 checkout_url = NULL, expires_at = $3,
                                 updated_at = clock_timestamp()
                             WHERE user_id = $1 AND id = $2",
                        )
                        .bind(user_id)
                        .bind(previous_id)
                        .bind(expires_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        context.attempt_id = previous_id;
                        return Ok(BeginCheckoutResult::Retry(context));
                    }
                    sqlx::query(
                        "INSERT INTO billing_checkout_session
                           (id, user_id, idempotency_key, billing_period, status, expires_at)
                         VALUES ($1, $2, $3, $4, 'creating', $5)",
                    )
                    .bind(attempt_id)
                    .bind(user_id)
                    .bind(idempotency_key)
                    .bind(billing_period.as_str())
                    .bind(expires_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    context.attempt_id = attempt_id;
                    Ok(BeginCheckoutResult::Created(context))
                })
            })
            .await
    }
}

impl BillingStore for PgBillingRepository {
    fn subscription_for_user(
        &self,
        user_id: Uuid,
    ) -> BillingStoreFuture<'_, Option<SubscriptionRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                "SELECT plan, provider, billing_period, status, cancel_at_period_end,
                        current_period_starts_at, current_period_ends_at,
                        trial_ends_at, canceled_at
                 FROM user_subscription WHERE user_id = $1",
            )
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| map_subscription(&row)).transpose()
        })
    }

    fn customer_for_user(&self, user_id: Uuid) -> BillingStoreFuture<'_, Option<String>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query_scalar(
                "SELECT stripe_customer_id FROM billing_customer
                 WHERE user_id = $1 AND stripe_customer_deleted_at IS NULL",
            )
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)
            .map_err(BillingStoreError::from)
        })
    }

    fn begin_checkout(
        &self,
        input: BeginCheckoutInput,
    ) -> BillingStoreFuture<'_, BeginCheckoutResult> {
        Box::pin(self.begin_checkout_transaction(input))
    }

    fn begin_customer_creation(
        &self,
        attempt_id: Uuid,
    ) -> BillingStoreFuture<'_, CustomerCreation> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let row = sqlx::query(
                            "UPDATE billing_checkout_session
                             SET customer_creation_started_at =
                                   COALESCE(customer_creation_started_at, clock_timestamp())
                             WHERE id = $1
                             RETURNING id, user_id, customer_creation_started_at,
                                       created_customer_id, customer_erased_at",
                        )
                        .bind(attempt_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .ok_or(BillingStoreError::CheckoutAttemptMissing)?;
                        let creation = map_customer_creation(&row)?;
                        sqlx::query(
                            "INSERT INTO outbox_event
                               (id, event_type, aggregate_id, available_at)
                             VALUES ($1, 'billing.customer.reconcile', $2, $3)
                             ON CONFLICT (event_type, aggregate_id) DO UPDATE
                             SET status = 'pending', attempts = 0,
                               available_at = EXCLUDED.available_at,
                               locked_at = NULL, locked_by = NULL,
                               last_error_code = NULL, processed_at = NULL,
                               dead_lettered_at = NULL, resolved_at = NULL,
                               resolved_by = NULL, resolution_reason = NULL
                             WHERE outbox_event.status IN ('completed', 'discarded')",
                        )
                        .bind(Uuid::new_v4())
                        .bind(creation.id)
                        .bind(
                            creation.customer_creation_started_at
                                + TimeDelta::hours(CUSTOMER_CREATE_SAFETY_HOURS),
                        )
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(creation)
                    })
                })
                .await
        })
    }

    fn record_created_customer(
        &self,
        attempt_id: Uuid,
        customer_id: String,
    ) -> BillingStoreFuture<'_, ()> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let updated = sqlx::query(
                            "UPDATE billing_checkout_session SET created_customer_id = $2
                             WHERE id = $1
                               AND (created_customer_id IS NULL OR created_customer_id = $2)",
                        )
                        .bind(attempt_id)
                        .bind(customer_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if updated.rows_affected() != 1 {
                            return Err(BillingStoreError::CheckoutCustomerConflict);
                        }
                        sqlx::query(
                            "UPDATE outbox_event SET available_at = clock_timestamp()
                             WHERE event_type = 'billing.customer.reconcile'
                               AND aggregate_id = $1 AND status = 'pending'",
                        )
                        .bind(attempt_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(())
                    })
                })
                .await
        })
    }

    fn save_customer(&self, user_id: Uuid, customer_id: String) -> BillingStoreFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let saved = sqlx::query(
                            "INSERT INTO billing_customer (user_id, stripe_customer_id)
                             SELECT user_id, $2 FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL
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
                        .bind(&customer_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if saved.is_none() {
                            return Ok(false);
                        }
                        sqlx::query(
                            "DELETE FROM outbox_event AS event
                             USING billing_checkout_session AS checkout
                             WHERE event.event_type = 'billing.customer.reconcile'
                               AND event.aggregate_id = checkout.id
                               AND checkout.user_id = $1
                               AND checkout.created_customer_id = $2
                               AND event.status IN ('pending', 'completed', 'discarded')",
                        )
                        .bind(user_id)
                        .bind(customer_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn mark_checkout_open(
        &self,
        attempt_id: Uuid,
        session: PersistedCheckoutSession,
    ) -> BillingStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let result = sqlx::query(
                "UPDATE billing_checkout_session
                 SET stripe_session_id = $2, checkout_url = $3, status = 'open',
                     expires_at = $4, updated_at = clock_timestamp()
                 WHERE id = $1 AND status = 'creating'",
            )
            .bind(attempt_id)
            .bind(session.session_id)
            .bind(session.url)
            .bind(session.expires_at)
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(result.rows_affected() == 1)
        })
    }

    fn mark_checkout_failed(&self, attempt_id: Uuid) -> BillingStoreFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                "UPDATE billing_checkout_session
                 SET status = 'failed', checkout_url = NULL, updated_at = clock_timestamp()
                 WHERE id = $1 AND status = 'creating'",
            )
            .bind(attempt_id)
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }

    fn customer_creations_for_erasure(
        &self,
        user_id: Uuid,
    ) -> BillingStoreFuture<'_, Vec<CustomerCreation>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let rows = sqlx::query(
                "SELECT id, user_id, customer_creation_started_at,
                        created_customer_id, customer_erased_at
                 FROM billing_checkout_session
                 WHERE user_id = $1 AND customer_creation_started_at IS NOT NULL
                   AND customer_erased_at IS NULL
                 ORDER BY id LIMIT 50",
            )
            .bind(user_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            rows.iter().map(map_customer_creation).collect()
        })
    }

    fn mark_created_customer_erased(&self, attempt_id: Uuid) -> BillingStoreFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                "UPDATE billing_checkout_session
                 SET customer_erased_at = clock_timestamp() WHERE id = $1",
            )
            .bind(attempt_id)
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }
}

async fn has_active_subscription(
    connection: &mut PgConnection,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, BillingStoreError> {
    let active: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM user_subscription
         WHERE user_id = $1 AND plan = 'premium'
           AND (
             (provider IS NULL AND (current_period_ends_at IS NULL OR current_period_ends_at > $2))
             OR
             (provider = 'stripe' AND status IN ('trialing', 'active', 'past_due')
               AND (current_period_ends_at IS NULL OR current_period_ends_at > $2))
           )
         LIMIT 1",
    )
    .bind(user_id)
    .bind(now)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(active.is_some())
}

async fn checkout_context(
    connection: &mut PgConnection,
    user_id: Uuid,
    attempt_id: Option<Uuid>,
) -> Result<Option<CheckoutContext>, BillingStoreError> {
    let row = sqlx::query(
        "SELECT plan.trial_days,
           CASE WHEN customer.stripe_customer_deleted_at IS NULL
             THEN customer.stripe_customer_id ELSE NULL END AS stripe_customer_id,
           customer.trial_used_at
         FROM subscription_plan AS plan
         LEFT JOIN billing_customer AS customer ON customer.user_id = $1
         WHERE plan.code = 'premium' AND plan.is_active = true",
    )
    .bind(user_id)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?;
    row.map(|row| {
        Ok(CheckoutContext {
            attempt_id: attempt_id.unwrap_or_else(Uuid::nil),
            stripe_customer_id: row.try_get("stripe_customer_id").map_err(map_sqlx_error)?,
            trial_days: row.try_get("trial_days").map_err(map_sqlx_error)?,
            trial_used: row
                .try_get::<Option<DateTime<Utc>>, _>("trial_used_at")
                .map_err(map_sqlx_error)?
                .is_some(),
        })
    })
    .transpose()
}

fn map_subscription(row: &sqlx::postgres::PgRow) -> Result<SubscriptionRow, BillingStoreError> {
    let provider: Option<String> = row.try_get("provider").map_err(map_sqlx_error)?;
    let provider = match provider.as_deref() {
        None => None,
        Some("stripe") => Some(SubscriptionProvider::Stripe),
        Some(_) => return Err(BillingStoreError::InvalidStoredData),
    };
    let period: Option<String> = row.try_get("billing_period").map_err(map_sqlx_error)?;
    let status: Option<String> = row.try_get("status").map_err(map_sqlx_error)?;
    Ok(SubscriptionRow {
        plan: row.try_get("plan").map_err(map_sqlx_error)?,
        provider,
        billing_period: period
            .map(|value| BillingPeriod::parse(&value).ok_or(BillingStoreError::InvalidStoredData))
            .transpose()?,
        status: status
            .map(|value| {
                StripeSubscriptionStatus::parse(&value).ok_or(BillingStoreError::InvalidStoredData)
            })
            .transpose()?,
        cancel_at_period_end: row
            .try_get("cancel_at_period_end")
            .map_err(map_sqlx_error)?,
        current_period_starts_at: row
            .try_get("current_period_starts_at")
            .map_err(map_sqlx_error)?,
        current_period_ends_at: row
            .try_get("current_period_ends_at")
            .map_err(map_sqlx_error)?,
        trial_ends_at: row.try_get("trial_ends_at").map_err(map_sqlx_error)?,
        canceled_at: row.try_get("canceled_at").map_err(map_sqlx_error)?,
    })
}

fn map_customer_creation(
    row: &sqlx::postgres::PgRow,
) -> Result<CustomerCreation, BillingStoreError> {
    Ok(CustomerCreation {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
        customer_creation_started_at: row
            .try_get("customer_creation_started_at")
            .map_err(map_sqlx_error)?,
        created_customer_id: row.try_get("created_customer_id").map_err(map_sqlx_error)?,
        customer_erased_at: row.try_get("customer_erased_at").map_err(map_sqlx_error)?,
    })
}
