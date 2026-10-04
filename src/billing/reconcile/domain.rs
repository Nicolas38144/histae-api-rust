use crate::billing::domain::StripeSubscriptionStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationKind {
    Subscription,
    CustomerCreation,
}

impl ReconciliationKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::CustomerCreation => "customer_creation",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReconciliationItem {
    pub event_id: Uuid,
    pub user_id: Uuid,
    pub kind: ReconciliationKind,
    pub attempts: u16,
    pub last_error_code: Option<String>,
    pub created_at: String,
    pub dead_lettered_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconciliationCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationRow {
    pub event_id: Uuid,
    pub user_id: Uuid,
    pub kind: ReconciliationKind,
    pub attempts: u16,
    pub last_error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dead_lettered_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionContext {
    pub user_id: Uuid,
    pub stripe_customer_id: String,
    pub projection_version: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerCreationContext {
    pub attempt_id: Uuid,
    pub user_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub created_customer_id: Option<String>,
    pub mapped_customer_id: Option<String>,
    pub customer_erased_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationApplyState {
    Applied,
    Stale,
    NotFound,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconciliationApplyResult {
    pub state: ReconciliationApplyState,
    pub previous_status: Option<StripeSubscriptionStatus>,
    pub status: Option<StripeSubscriptionStatus>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomerRecoveryResult {
    Recovered,
    Cleared,
    AlreadyResolved,
    NotFound,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripeCustomerState {
    pub id: String,
    pub deleted: bool,
    pub metadata_user_id: Option<Uuid>,
    pub metadata_attempt_id: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripeCollection<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}
