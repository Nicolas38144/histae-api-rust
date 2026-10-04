use std::fmt;
use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::domain::{
    BeginCheckoutResult, BillingPeriod, CustomerCreation, PersistedCheckoutSession, SubscriptionRow,
};
use crate::infra::postgres::DatabaseError;

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
