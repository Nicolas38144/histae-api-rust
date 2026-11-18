use super::{
    domain::{BillingError, BillingPeriod, SubscriptionProvider, SubscriptionView},
    store::{BillingStore, BillingStoreFuture},
};
use crate::infra::postgres::{Database, map_sqlx_error};
use chrono::{DateTime, Utc};
use sqlx::Row as _;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgBillingRepository {
    database: Database,
}

impl PgBillingRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl BillingStore for PgBillingRepository {
    fn effective_subscription_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> BillingStoreFuture<'_, Option<SubscriptionView>> {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT provider, status, access_granted, billing_period, cancel_at_period_end,
                        current_period_starts_at, current_period_ends_at, canceled_at
                 FROM effective_subscription_at($1, $2)",
            )
            .bind(user_id)
            .bind(now)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            row.as_ref().map(effective_subscription_view).transpose()
        })
    }
}

fn effective_subscription_view(
    row: &sqlx::postgres::PgRow,
) -> Result<SubscriptionView, BillingError> {
    use crate::identity::mobile::domain::wire_timestamp;
    let provider: Option<String> = row.try_get("provider").map_err(map_sqlx_error)?;
    let provider = provider
        .map(|value| SubscriptionProvider::parse(&value).ok_or(BillingError::InvalidStoredData))
        .transpose()?;
    let billing_period: Option<String> = row.try_get("billing_period").map_err(map_sqlx_error)?;
    let access_granted: bool = row.try_get("access_granted").map_err(map_sqlx_error)?;
    let date = |name| -> Result<Option<String>, BillingError> {
        Ok(row
            .try_get::<Option<DateTime<Utc>>, _>(name)
            .map_err(map_sqlx_error)?
            .map(wire_timestamp))
    };
    Ok(SubscriptionView {
        plan: if access_granted { "premium" } else { "free" },
        provider,
        status: row.try_get("status").map_err(map_sqlx_error)?,
        access_granted,
        billing_period: billing_period
            .map(|value| BillingPeriod::parse(&value).ok_or(BillingError::InvalidStoredData))
            .transpose()?,
        cancel_at_period_end: row
            .try_get("cancel_at_period_end")
            .map_err(map_sqlx_error)?,
        current_period_starts_at: date("current_period_starts_at")?,
        current_period_ends_at: date("current_period_ends_at")?,
        canceled_at: date("canceled_at")?,
        customer_portal_available: false,
        trial_ends_at: None,
        management_url: provider.map(|provider| provider.management_url().to_owned()),
    })
}
