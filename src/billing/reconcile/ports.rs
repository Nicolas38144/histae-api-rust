use super::{
    CustomerCreationContext, CustomerRecoveryResult, ReconciliationApplyResult,
    ReconciliationCursor, ReconciliationKind, ReconciliationRow, StripeCollection,
    StripeCustomerState, SubscriptionContext,
};
use crate::billing::{stripe::StripeError, webhook::SubscriptionProjection};
use crate::infra::postgres::DatabaseError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin};
use uuid::Uuid;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationStoreError {
    Database(DatabaseError),
    InvalidStoredData,
}

impl From<DatabaseError> for ReconciliationStoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl fmt::Display for ReconciliationStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Database(error) => error.safe_code(),
            Self::InvalidStoredData => "invalid_billing_data",
        })
    }
}

impl std::error::Error for ReconciliationStoreError {}

pub type ReconciliationStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ReconciliationStoreError>> + Send + 'a>>;

pub trait BillingReconciliationStore: Send + Sync {
    fn schedule_due(&self, now: DateTime<Utc>, limit: u16) -> ReconciliationStoreFuture<'_, u32>;
    fn subscription_context(
        &self,
        user_id: Uuid,
    ) -> ReconciliationStoreFuture<'_, Option<SubscriptionContext>>;
    fn customer_creation_context(
        &self,
        attempt_id: Uuid,
    ) -> ReconciliationStoreFuture<'_, Option<CustomerCreationContext>>;
    fn apply_subscription(
        &self,
        context: SubscriptionContext,
        projection: Option<SubscriptionProjection>,
        snapshot_at: DateTime<Utc>,
        next_due_at: DateTime<Utc>,
        customer_deleted: bool,
    ) -> ReconciliationStoreFuture<'_, ReconciliationApplyResult>;
    fn recover_customer_creation(
        &self,
        attempt_id: Uuid,
        customer_id: Option<String>,
    ) -> ReconciliationStoreFuture<'_, CustomerRecoveryResult>;
    fn list(
        &self,
        kind: Option<ReconciliationKind>,
        limit: u32,
        cursor: Option<ReconciliationCursor>,
    ) -> ReconciliationStoreFuture<'_, Vec<ReconciliationRow>>;
}

pub type ReconciliationGatewayFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, StripeError>> + Send + 'a>>;

pub trait StripeReconciliationGateway: Send + Sync {
    fn retrieve_customer(
        &self,
        customer_id: &str,
    ) -> ReconciliationGatewayFuture<'_, StripeCustomerState>;
    fn list_customer_subscriptions(
        &self,
        customer_id: &str,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<Value>>;
    fn search_customers_by_attempt(
        &self,
        attempt_id: Uuid,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<StripeCustomerState>>;
    fn list_customers_created_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<StripeCustomerState>>;
}
