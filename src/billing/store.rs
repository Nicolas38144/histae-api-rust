use super::domain::SubscriptionView;
use crate::infra::postgres::DatabaseError;
use chrono::{DateTime, Utc};
use std::{fmt, future::Future, pin::Pin};
use uuid::Uuid;

pub type BillingStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BillingStoreError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingStoreError {
    Database(DatabaseError),
    InvalidStoredData,
}

impl From<DatabaseError> for BillingStoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl fmt::Display for BillingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database(e) => e.safe_code(),
            Self::InvalidStoredData => "invalid_billing_data",
        })
    }
}

impl std::error::Error for BillingStoreError {}

pub trait BillingStore: Send + Sync {
    fn effective_subscription_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> BillingStoreFuture<'_, Option<SubscriptionView>>;
}
