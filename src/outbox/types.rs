use std::fmt;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboxEventType {
    PhotoDelete,
    NotificationPush,
    AccountErase,
    BillingSubscriptionReconcile,
    BillingCustomerReconcile,
    Unsupported(String),
}

impl OutboxEventType {
    pub fn parse(value: String) -> Self {
        match value.as_str() {
            "photo.delete" => Self::PhotoDelete,
            "notification.push" => Self::NotificationPush,
            "account.erase" => Self::AccountErase,
            "billing.subscription.reconcile" => Self::BillingSubscriptionReconcile,
            "billing.customer.reconcile" => Self::BillingCustomerReconcile,
            _ => Self::Unsupported(value),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::PhotoDelete => "photo.delete",
            Self::NotificationPush => "notification.push",
            Self::AccountErase => "account.erase",
            Self::BillingSubscriptionReconcile => "billing.subscription.reconcile",
            Self::BillingCustomerReconcile => "billing.customer.reconcile",
            Self::Unsupported(value) => value,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

impl fmt::Display for OutboxEventType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxStatus {
    Pending,
    Processing,
    Completed,
    DeadLetter,
    Discarded,
}

impl OutboxStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "processing" => Some(Self::Processing),
            "completed" => Some(Self::Completed),
            "dead_letter" => Some(Self::DeadLetter),
            "discarded" => Some(Self::Discarded),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEvent {
    pub id: Uuid,
    pub event_type: OutboxEventType,
    pub aggregate_id: Uuid,
    pub payload: Map<String, Value>,
    pub status: OutboxStatus,
    pub attempts: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewOutboxEvent {
    pub event_type: OutboxEventType,
    pub aggregate_id: Uuid,
    pub payload: Map<String, Value>,
}

impl NewOutboxEvent {
    pub fn empty(event_type: OutboxEventType, aggregate_id: Uuid) -> Self {
        Self {
            event_type,
            aggregate_id,
            payload: Map::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryResult {
    Pending,
    DeadLetter,
    NotOwned,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutboxWorkerResult {
    pub claimed: u32,
    pub completed: u32,
    pub deferred: u32,
    pub retried: u32,
    pub dead_lettered: u32,
    pub purged: u64,
    pub purge_batches: u32,
    pub work_remaining: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Completed,
    Deferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchFailure {
    pub code: &'static str,
    pub permanent: bool,
}

impl DispatchFailure {
    pub const fn transient(code: &'static str) -> Self {
        Self {
            code,
            permanent: false,
        }
    }

    pub const fn permanent(code: &'static str) -> Self {
        Self {
            code,
            permanent: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PurgeResult {
    pub purged: u64,
    pub batches: u32,
    pub work_remaining: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimWindow {
    pub now: DateTime<Utc>,
    pub stale_before: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_known_types_and_keeps_unknown_types_explicit() {
        assert_eq!(
            OutboxEventType::parse("photo.delete".to_owned()),
            OutboxEventType::PhotoDelete
        );
        let unknown = OutboxEventType::parse("future.effect".to_owned());
        assert_eq!(unknown.as_str(), "future.effect");
        assert!(!unknown.is_supported());
    }
}
