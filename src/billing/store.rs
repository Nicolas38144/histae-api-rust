use super::domain::{BillingError, SubscriptionView};
use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type BillingStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BillingError>> + Send + 'a>>;

pub trait BillingStore: Send + Sync {
    fn effective_subscription_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> BillingStoreFuture<'_, Option<SubscriptionView>>;
}
