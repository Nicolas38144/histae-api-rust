use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::mobile::domain::wire_timestamp;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DevicePlatform {
    Ios,
    Android,
}

impl DevicePlatform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ios" => Some(Self::Ios),
            "android" => Some(Self::Android),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRecord {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub platform: DevicePlatform,
    pub app_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRegistration {
    pub token: String,
    pub platform: DevicePlatform,
    pub app_version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicDevice {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub platform: DevicePlatform,
    pub app_version: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl From<DeviceRecord> for PublicDevice {
    fn from(value: DeviceRecord) -> Self {
        Self {
            id: value.id,
            session_id: value.session_id,
            platform: value.platform,
            app_version: value.app_version,
            created_at: wire_timestamp(value.created_at),
            last_used_at: value.last_used_at.map(wire_timestamp),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NotificationIntent {
    NewMatch {
        match_id: Uuid,
    },
    NewMessage {
        match_id: Uuid,
        message_id: Uuid,
        sender_id: Uuid,
    },
    BillingPaymentFailed {
        invoice_id: String,
    },
    SubscriptionTrialEnding {
        subscription_id: String,
        trial_ends_at: DateTime<Utc>,
    },
}

impl NotificationIntent {
    pub const fn notification_type(&self) -> &'static str {
        match self {
            Self::NewMatch { .. } => "new_match",
            Self::NewMessage { .. } => "new_message",
            Self::BillingPaymentFailed { .. } => "billing_payment_failed",
            Self::SubscriptionTrialEnding { .. } => "subscription_trial_ending",
        }
    }

    pub fn payload(&self) -> Value {
        match self {
            Self::NewMatch { match_id } => json!({ "match_id": match_id }),
            Self::NewMessage {
                match_id,
                message_id,
                sender_id,
            } => json!({
                "match_id": match_id,
                "message_id": message_id,
                "sender_id": sender_id,
            }),
            Self::BillingPaymentFailed { .. } | Self::SubscriptionTrialEnding { .. } => {
                json!({})
            }
        }
    }

    pub fn billing_reference(&self) -> Option<&str> {
        match self {
            Self::BillingPaymentFailed { invoice_id } => Some(invoice_id),
            Self::SubscriptionTrialEnding {
                subscription_id, ..
            } => Some(subscription_id),
            Self::NewMatch { .. } | Self::NewMessage { .. } => None,
        }
    }

    pub fn billing_trial_ends_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::SubscriptionTrialEnding { trial_ends_at, .. } => Some(*trial_ends_at),
            Self::NewMatch { .. } | Self::NewMessage { .. } | Self::BillingPaymentFailed { .. } => {
                None
            }
        }
    }
}
