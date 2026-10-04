use super::{
    InvoiceProjection, StripeWebhookStore, SubscriptionProjection, WebhookCommand, WebhookEffect,
    WebhookProcessResult, WebhookStoreError, WebhookStoreFuture, status_name,
};
use crate::billing::domain::StripeSubscriptionStatus;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::notifications::{domain::NotificationIntent, enqueue_notification};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
#[derive(Clone)]
pub struct PgStripeWebhookStore {
    database: Database,
}

impl PgStripeWebhookStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl StripeWebhookStore for PgStripeWebhookStore {
    fn processed(&self, event_id: &str) -> WebhookStoreFuture<'_, bool> {
        let event_id = event_id.to_owned();
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query_scalar::<_, i32>("SELECT 1 FROM stripe_webhook_event WHERE id = $1")
                .bind(event_id)
                .fetch_optional(&mut *connection)
                .await
                .map(|row| row.is_some())
                .map_err(map_sqlx_error)
                .map_err(WebhookStoreError::from)
        })
    }

    fn apply(&self, command: WebhookCommand) -> WebhookStoreFuture<'_, WebhookProcessResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let metadata = command.metadata();
                        let inserted: Option<String> = sqlx::query_scalar(
                            "INSERT INTO stripe_webhook_event
                               (id, event_type, object_id, livemode, api_version, stripe_created_at)
                             VALUES ($1, $2, $3, $4, $5, $6)
                             ON CONFLICT (id) DO NOTHING RETURNING id",
                        )
                        .bind(&metadata.id)
                        .bind(&metadata.event_type)
                        .bind(&metadata.object_id)
                        .bind(metadata.livemode)
                        .bind(&metadata.api_version)
                        .bind(metadata.created_at)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if inserted.is_none() {
                            return Ok(WebhookProcessResult::Duplicate);
                        }
                        match apply_command(connection, command).await {
                            Ok(effect) => Ok(WebhookProcessResult::Applied(effect)),
                            Err(ApplyError::InactiveAccount) => {
                                Ok(WebhookProcessResult::Applied(None))
                            }
                            Err(ApplyError::InvalidMapping) => {
                                Err(WebhookStoreError::InvalidMapping)
                            }
                            Err(ApplyError::Database(error)) => {
                                Err(WebhookStoreError::Database(error))
                            }
                        }
                    })
                })
                .await
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyError {
    Database(DatabaseError),
    InvalidMapping,
    InactiveAccount,
}

impl From<DatabaseError> for ApplyError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

async fn apply_command(
    connection: &mut PgConnection,
    command: WebhookCommand,
) -> Result<Option<WebhookEffect>, ApplyError> {
    match command {
        WebhookCommand::Receipt(_) => Ok(None),
        WebhookCommand::Subscription {
            metadata,
            projection,
            notify_trial_ending,
        } => {
            let user_id = resolve_user(connection, &projection).await?;
            upsert_subscription(connection, user_id, &projection).await?;
            if notify_trial_ending && let Some(trial_ends_at) = projection.trial_ends_at {
                enqueue_notification(
                    connection,
                    user_id,
                    &metadata.id,
                    &NotificationIntent::SubscriptionTrialEnding {
                        subscription_id: projection.stripe_subscription_id.clone(),
                        trial_ends_at,
                    },
                )
                .await?;
            }
            Ok(Some(WebhookEffect {
                user_id,
                subscription_status: Some(projection.status),
            }))
        }
        WebhookCommand::Invoice {
            metadata,
            subscription,
            invoice,
            notify_payment_failed,
        } => {
            let user_id = resolve_user(connection, &subscription).await?;
            upsert_subscription(connection, user_id, &subscription).await?;
            upsert_invoice(connection, user_id, &invoice).await?;
            if notify_payment_failed {
                enqueue_notification(
                    connection,
                    user_id,
                    &metadata.id,
                    &NotificationIntent::BillingPaymentFailed {
                        invoice_id: invoice.stripe_invoice_id.clone(),
                    },
                )
                .await?;
            }
            Ok(Some(WebhookEffect {
                user_id,
                subscription_status: Some(subscription.status),
            }))
        }
        WebhookCommand::Checkout {
            session_id, status, ..
        } => {
            let user_id: Option<Uuid> = sqlx::query_scalar(
                "UPDATE billing_checkout_session
                 SET status = $2, checkout_url = NULL, updated_at = clock_timestamp()
                 WHERE stripe_session_id = $1 AND status IN ('open', 'creating')
                 RETURNING user_id",
            )
            .bind(session_id)
            .bind(status)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(user_id.map(|user_id| WebhookEffect {
                user_id,
                subscription_status: None,
            }))
        }
        WebhookCommand::CustomerDeleted {
            metadata,
            customer_id,
        } => mark_customer_deleted(connection, &customer_id, metadata.created_at).await,
    }
}

async fn resolve_user(
    connection: &mut PgConnection,
    projection: &SubscriptionProjection,
) -> Result<Uuid, ApplyError> {
    let mapped: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM billing_customer WHERE stripe_customer_id = $1")
            .bind(&projection.stripe_customer_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    let owner = mapped.or(projection.metadata_user_id);
    if let Some(owner) = owner {
        require_active_account(connection, owner).await?;
    }
    if let Some(mapped) = mapped {
        if projection
            .metadata_user_id
            .is_some_and(|metadata| metadata != mapped)
        {
            return Err(ApplyError::InvalidMapping);
        }
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM billing_customer
             WHERE stripe_customer_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(&projection.stripe_customer_id)
        .bind(mapped)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        return locked.ok_or(ApplyError::InvalidMapping);
    }
    let user_id = projection
        .metadata_user_id
        .ok_or(ApplyError::InvalidMapping)?;
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO billing_customer (user_id, stripe_customer_id)
         SELECT user_id, $2 FROM user_account WHERE user_id = $1 AND deleted_at IS NULL
         ON CONFLICT (user_id) DO UPDATE SET updated_at = clock_timestamp()
         WHERE billing_customer.stripe_customer_id = EXCLUDED.stripe_customer_id
         RETURNING user_id",
    )
    .bind(user_id)
    .bind(&projection.stripe_customer_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    inserted.ok_or(ApplyError::InvalidMapping)
}

async fn require_active_account(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> Result<(), ApplyError> {
    let row = sqlx::query("SELECT deleted_at FROM user_account WHERE user_id = $1 FOR SHARE")
        .bind(user_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let Some(row) = row else {
        return Err(ApplyError::InvalidMapping);
    };
    let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").map_err(map_sqlx_error)?;
    if deleted_at.is_some() {
        return Err(ApplyError::InactiveAccount);
    }
    Ok(())
}

async fn upsert_subscription(
    connection: &mut PgConnection,
    user_id: Uuid,
    input: &SubscriptionProjection,
) -> Result<(), ApplyError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO user_subscription (
           user_id, plan, provider, provider_subscription_id, provider_price_id,
           billing_period, status, cancel_at_period_end, current_period_starts_at,
           current_period_ends_at, trial_ends_at, canceled_at, provider_event_created_at,
           projection_version, provider_snapshot_at, updated_at
         ) VALUES ($1, 'premium', 'stripe', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 1, $11, clock_timestamp())
         ON CONFLICT (user_id) DO UPDATE SET
           plan = 'premium', provider = 'stripe', provider_subscription_id = EXCLUDED.provider_subscription_id,
           provider_price_id = EXCLUDED.provider_price_id, billing_period = EXCLUDED.billing_period,
           status = EXCLUDED.status, cancel_at_period_end = EXCLUDED.cancel_at_period_end,
           current_period_starts_at = EXCLUDED.current_period_starts_at,
           current_period_ends_at = EXCLUDED.current_period_ends_at,
           trial_ends_at = EXCLUDED.trial_ends_at, canceled_at = EXCLUDED.canceled_at,
           provider_event_created_at = EXCLUDED.provider_event_created_at,
           provider_snapshot_at = GREATEST(user_subscription.provider_snapshot_at, EXCLUDED.provider_snapshot_at),
           projection_version = user_subscription.projection_version + 1,
           updated_at = clock_timestamp()
         WHERE (
             user_subscription.provider_subscription_id IS NULL
             OR user_subscription.provider_subscription_id = EXCLUDED.provider_subscription_id
             OR user_subscription.status NOT IN ('trialing', 'active', 'past_due')
           )
           AND (
             user_subscription.provider_event_created_at IS NULL
             OR user_subscription.provider_event_created_at < EXCLUDED.provider_event_created_at
             OR (
               user_subscription.provider_event_created_at = EXCLUDED.provider_event_created_at
               AND (
                 user_subscription.status NOT IN ('canceled', 'unpaid', 'incomplete_expired')
                 OR EXCLUDED.status IN ('canceled', 'unpaid', 'incomplete_expired')
               )
             )
           )
           AND (
             user_subscription.provider_snapshot_at IS NULL
             OR user_subscription.provider_snapshot_at <= EXCLUDED.provider_snapshot_at
           )
         RETURNING user_id",
    )
    .bind(user_id)
    .bind(&input.stripe_subscription_id)
    .bind(&input.stripe_price_id)
    .bind(input.billing_period.as_str())
    .bind(status_name(input.status))
    .bind(input.cancel_at_period_end)
    .bind(input.current_period_starts_at)
    .bind(input.current_period_ends_at)
    .bind(input.trial_ends_at)
    .bind(input.canceled_at)
    .bind(input.event_created_at)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if updated.is_none() {
        let current: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT provider_subscription_id, provider_event_created_at
             FROM user_subscription WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        let stale_same_subscription = current.is_some_and(|(subscription_id, event_at)| {
            subscription_id == input.stripe_subscription_id
                && event_at.is_some_and(|event_at| event_at >= input.event_created_at)
        });
        if !stale_same_subscription {
            return Err(ApplyError::InvalidMapping);
        }
    }
    if let Some(trial_starts_at) = input.trial_starts_at {
        sqlx::query(
            "UPDATE billing_customer
             SET trial_used_at = COALESCE(trial_used_at, $2), updated_at = clock_timestamp()
             WHERE user_id = $1 AND stripe_customer_id = $3",
        )
        .bind(user_id)
        .bind(trial_starts_at)
        .bind(&input.stripe_customer_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    Ok(())
}

async fn upsert_invoice(
    connection: &mut PgConnection,
    user_id: Uuid,
    input: &InvoiceProjection,
) -> Result<(), ApplyError> {
    sqlx::query(
        "INSERT INTO billing_invoice (
           stripe_invoice_id, user_id, stripe_customer_id, stripe_subscription_id,
           status, currency, amount_due, amount_paid, amount_remaining,
           period_starts_at, period_ends_at, paid_at, created_at,
           provider_event_created_at, updated_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, clock_timestamp())
         ON CONFLICT (stripe_invoice_id) DO UPDATE SET
           user_id = EXCLUDED.user_id, stripe_customer_id = EXCLUDED.stripe_customer_id,
           stripe_subscription_id = EXCLUDED.stripe_subscription_id, status = EXCLUDED.status,
           currency = EXCLUDED.currency, amount_due = EXCLUDED.amount_due,
           amount_paid = EXCLUDED.amount_paid, amount_remaining = EXCLUDED.amount_remaining,
           period_starts_at = EXCLUDED.period_starts_at, period_ends_at = EXCLUDED.period_ends_at,
           paid_at = EXCLUDED.paid_at, provider_event_created_at = EXCLUDED.provider_event_created_at,
           updated_at = clock_timestamp()
         WHERE billing_invoice.provider_event_created_at IS NULL
           OR billing_invoice.provider_event_created_at < EXCLUDED.provider_event_created_at
           OR (
             billing_invoice.provider_event_created_at = EXCLUDED.provider_event_created_at
             AND (
               billing_invoice.status NOT IN ('paid', 'void', 'uncollectible')
               OR EXCLUDED.status IN ('paid', 'void', 'uncollectible')
             )
           )",
    )
    .bind(&input.stripe_invoice_id)
    .bind(user_id)
    .bind(&input.stripe_customer_id)
    .bind(&input.stripe_subscription_id)
    .bind(&input.status)
    .bind(&input.currency)
    .bind(input.amount_due)
    .bind(input.amount_paid)
    .bind(input.amount_remaining)
    .bind(input.period_starts_at)
    .bind(input.period_ends_at)
    .bind(input.paid_at)
    .bind(input.created_at)
    .bind(input.event_created_at)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn mark_customer_deleted(
    connection: &mut PgConnection,
    customer_id: &str,
    event_created_at: DateTime<Utc>,
) -> Result<Option<WebhookEffect>, ApplyError> {
    let user_id: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM billing_customer WHERE stripe_customer_id = $1")
            .bind(customer_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    require_active_account(connection, user_id).await?;
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM billing_customer
         WHERE stripe_customer_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(customer_id)
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if locked.is_none() {
        return Err(ApplyError::InvalidMapping);
    }
    sqlx::query(
        "UPDATE user_subscription
         SET status = 'canceled', cancel_at_period_end = false,
           canceled_at = COALESCE(canceled_at, $2),
           provider_event_created_at = GREATEST(provider_event_created_at, $2),
           provider_snapshot_at = GREATEST(provider_snapshot_at, $2),
           projection_version = projection_version + 1, updated_at = clock_timestamp()
         WHERE user_id = $1 AND provider = 'stripe'
           AND (provider_snapshot_at IS NULL OR provider_snapshot_at <= $2)",
    )
    .bind(user_id)
    .bind(event_created_at)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE billing_checkout_session
         SET status = 'failed', checkout_url = NULL, updated_at = clock_timestamp()
         WHERE user_id = $1 AND status IN ('creating', 'open')",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE billing_customer
         SET stripe_customer_deleted_at = clock_timestamp(), updated_at = clock_timestamp()
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(Some(WebhookEffect {
        user_id,
        subscription_status: Some(StripeSubscriptionStatus::Canceled),
    }))
}
