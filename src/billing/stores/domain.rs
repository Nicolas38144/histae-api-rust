use crate::billing::domain::BillingPeriod;
use crate::infra::postgres::DatabaseError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreProvider {
    AppStore,
    GooglePlay,
}
impl StoreProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AppStore => "app_store",
            Self::GooglePlay => "google_play",
        }
    }
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "app_store" => Ok(Self::AppStore),
            "google_play" => Ok(Self::GooglePlay),
            _ => Err(StoreError::Invalid),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSnapshot {
    pub provider: StoreProvider,
    pub original_id: String,
    pub transaction_id: String,
    pub product_id: String,
    pub base_plan_id: Option<String>,
    pub account_binding: String,
    pub environment: String,
    pub billing_period: BillingPeriod,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub auto_renew: bool,
    pub acknowledgement_required: bool,
    pub linked_reference_hash: Option<String>,
}

impl StoreSnapshot {
    pub fn valid(&self) -> bool {
        let binding_valid = match self.provider {
            StoreProvider::AppStore => {
                Uuid::parse_str(&self.account_binding).is_ok_and(|id| id.get_version_num() == 4)
            }
            StoreProvider::GooglePlay => {
                self.account_binding.len() == 64
                    && self
                        .account_binding
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            }
        };
        binding_valid
            && self.linked_reference_hash.as_ref().is_none_or(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && !self.original_id.is_empty()
            && self.original_id.len() <= 256
            && !self.transaction_id.is_empty()
            && self.transaction_id.len() <= 256
            && !self.product_id.is_empty()
            && self.product_id.len() <= 256
            && self.account_binding.len() <= 64
            && ["production", "sandbox"].contains(&self.environment.as_str())
            && [
                "pending",
                "active",
                "grace_period",
                "past_due",
                "on_hold",
                "paused",
                "canceled",
                "expired",
                "revoked",
            ]
            .contains(&self.status.as_str())
            && self.expires_at >= self.starts_at
    }
}

#[derive(Deserialize)]
pub struct StoreNotification {
    pub event_id: String,
    pub reference: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoreOffer {
    pub billing_period: BillingPeriod,
    pub product_id: String,
    pub base_plan_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PurchaseIntent {
    pub provider: StoreProvider,
    pub account_binding: String,
    pub offers: Vec<StoreOffer>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    Unavailable,
    Invalid,
    Ownership,
    AlreadySubscribed,
    AccountUnavailable,
    Database(DatabaseError),
}
impl From<DatabaseError> for StoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "store_unavailable",
            Self::Invalid => "invalid_store_purchase",
            Self::Ownership => "store_purchase_account_conflict",
            Self::AlreadySubscribed => "subscription_already_active",
            Self::AccountUnavailable => "account_not_found",
            Self::Database(error) => error.safe_code(),
        })
    }
}
impl std::error::Error for StoreError {}
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

pub struct StoreAccount {
    pub id: Uuid,
    pub google_id: String,
}

pub trait StoreEvents: Send + Sync {
    fn subscription_changed(&self, user_id: Uuid) -> StoreFuture<'_, ()>;
}
