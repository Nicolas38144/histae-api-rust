use crate::infra::postgres::DatabaseError;
use serde::Serialize;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtpDeliveryState {
    Pending,
    Accepted,
    Sent,
    Failed,
    Unknown,
}

impl OtpDeliveryState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, DatabaseError> {
        match value {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "sent" => Ok(Self::Sent),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            _ => Err(DatabaseError::QueryFailed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtpDeliveryStart {
    Created(Uuid),
    Existing(OtpDeliveryState, Uuid),
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeginOtpDelivery {
    pub id: Uuid,
    pub phone_hash: String,
    pub otp_hash: String,
    pub idempotency_key: Uuid,
    pub ttl: Duration,
    pub settlement: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmsDeliveryEvent {
    pub delivery_id: Uuid,
    pub message_id: String,
    pub transaction_id: Option<String>,
    pub kind: SmsEventKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsEventKind {
    Sent,
    Undelivered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsEventOutcome {
    Applied,
    Ignored,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OtpDeliveryStates {
    pub pending: i32,
    pub accepted: i32,
    pub sent: i32,
    pub failed: i32,
    pub unknown: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OtpDeliverySnapshot {
    pub states: OtpDeliveryStates,
    pub awaiting_callback: i32,
    pub oldest_unresolved_age_seconds: Option<f64>,
    pub average_acceptance_ms: Option<f64>,
    pub average_sent_callback_ms: Option<f64>,
    pub average_failure_ms: Option<f64>,
    pub retention: &'static str,
    pub handset_delivery: &'static str,
}
