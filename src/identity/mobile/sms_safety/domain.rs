use std::{future::Future, pin::Pin};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::{config::SmsSafetyConfig, infra::postgres::DatabaseError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsSendRejection {
    Suspended,
    HourlyLimit,
    DailyLimit,
    AlreadyReserved,
    Unavailable,
}

pub type SmsPermitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), SmsSendRejection>> + Send + 'a>>;

/// A permit is consumed before the provider request. Uncertain outcomes are never refunded.
pub trait SmsSendGuard: Send + Sync {
    fn reserve<'a>(&'a self, delivery_id: Uuid, phone: &'a str) -> SmsPermitFuture<'a>;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SmsSafetySnapshot {
    pub provider_enabled: bool,
    pub suspended: bool,
    pub manual_suspended: bool,
    pub environment_suspended: bool,
    pub version: i64,
    pub hourly_limit_per_phone: u32,
    pub daily_limit_per_phone: u32,
    pub sends_last_5_minutes: i64,
    pub sends_last_hour: i64,
    pub sends_last_24_hours: i64,
    pub spike_threshold_5_minutes: u32,
    pub traffic_spike: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct SmsUsage {
    pub manual_suspended: bool,
    pub version: i64,
    pub last_5_minutes: i64,
    pub last_hour: i64,
    pub last_day: i64,
    pub previous_hour: i64,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsControlError {
    Forbidden,
    Conflict,
    Database(DatabaseError),
}

impl From<DatabaseError> for SmsControlError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl std::fmt::Display for SmsControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Forbidden => "admin_action_forbidden",
            Self::Conflict => "resource_changed",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for SmsControlError {}

pub type SmsSafetyFuture<'a, T, E = DatabaseError> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

pub trait SmsSafetyStore: Send + Sync {
    fn reserve<'a>(
        &'a self,
        delivery_id: Uuid,
        phone_hash: String,
        policy: SmsSafetyConfig,
    ) -> SmsSafetyFuture<'a, Result<(), SmsSendRejection>>;
    fn usage(&self) -> SmsSafetyFuture<'_, SmsUsage>;
    fn set_suspended(
        &self,
        actor: Uuid,
        suspended: bool,
        version: i64,
    ) -> SmsSafetyFuture<'_, (), SmsControlError>;
}
