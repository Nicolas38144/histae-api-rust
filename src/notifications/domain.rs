use crate::localization::Language;
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
    pub language: Option<Language>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRegistration {
    pub token: String,
    pub platform: DevicePlatform,
    pub app_version: Option<String>,
    pub language: Option<Language>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicDevice {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub platform: DevicePlatform,
    pub app_version: Option<String>,
    pub language: Option<Language>,
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
            language: value.language,
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
    MatchExpiring {
        match_id: Uuid,
        expires_at: DateTime<Utc>,
    },
    ContinuationRequested {
        match_id: Uuid,
        actor_id: Uuid,
    },
    ConversationContinued {
        match_id: Uuid,
    },
    PhotosRevealed {
        match_id: Uuid,
        reveal_version: i64,
    },
    FirstPeriodEnding {
        match_id: Uuid,
        expires_at: DateTime<Utc>,
    },
    DataRequestReplied {
        request_id: Uuid,
        response_status: String,
    },
    NewSession {
        session_id: Uuid,
    },
    BillingPaymentFailed {
        invoice_id: String,
    },
}

impl NotificationIntent {
    pub const fn kind(&self) -> NotificationType {
        match self {
            Self::NewMatch { .. } => NotificationType::NewMatch,
            Self::NewMessage { .. } => NotificationType::NewMessage,
            Self::MatchExpiring { .. } => NotificationType::MatchExpiring,
            Self::ContinuationRequested { .. } => NotificationType::ContinuationRequested,
            Self::ConversationContinued { .. } => NotificationType::ConversationContinued,
            Self::PhotosRevealed { .. } => NotificationType::PhotosRevealed,
            Self::FirstPeriodEnding { .. } => NotificationType::FirstPeriodEnding,
            Self::DataRequestReplied { .. } => NotificationType::DataRequestReplied,
            Self::NewSession { .. } => NotificationType::NewSession,
            Self::BillingPaymentFailed { .. } => NotificationType::BillingPaymentFailed,
        }
    }

    pub const fn notification_type(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn payload(&self) -> Value {
        match self {
            Self::NewMatch { match_id } | Self::ConversationContinued { match_id } => {
                json!({ "match_id": match_id })
            }
            Self::NewMessage {
                match_id,
                message_id,
                sender_id,
            } => {
                json!({ "match_id": match_id, "message_id": message_id, "sender_id": sender_id })
            }
            Self::MatchExpiring {
                match_id,
                expires_at,
            }
            | Self::FirstPeriodEnding {
                match_id,
                expires_at,
            } => json!({
                "match_id": match_id,
                "expires_at": expires_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            }),
            Self::ContinuationRequested { match_id, actor_id } => {
                json!({ "match_id": match_id, "actor_id": actor_id })
            }
            Self::PhotosRevealed {
                match_id,
                reveal_version,
            } => {
                json!({ "match_id": match_id, "reveal_version": reveal_version })
            }
            Self::DataRequestReplied {
                request_id,
                response_status,
            } => {
                json!({ "request_id": request_id, "response_status": response_status })
            }
            Self::NewSession { session_id } => json!({ "session_id": session_id }),
            Self::BillingPaymentFailed { .. } => json!({}),
        }
    }

    pub fn billing_reference(&self) -> Option<&str> {
        match self {
            Self::BillingPaymentFailed { invoice_id } => Some(invoice_id),
            _ => None,
        }
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::MatchExpiring { expires_at, .. } | Self::FirstPeriodEnding { expires_at, .. } => {
                Some(*expires_at)
            }
            _ => None,
        }
    }

    pub const fn excluded_session_id(&self) -> Option<Uuid> {
        match self {
            Self::NewSession { session_id } => Some(*session_id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationType {
    NewMatch,
    NewMessage,
    MatchExpiring,
    BillingPaymentFailed,
    ContinuationRequested,
    ConversationContinued,
    PhotosRevealed,
    FirstPeriodEnding,
    DataRequestReplied,
    NewSession,
}

impl NotificationType {
    /// Stable IDs shared with Android's user-configurable notification channels.
    pub const fn channel_id(self) -> &'static str {
        match self {
            Self::NewMessage => "histae_messages",
            Self::NewMatch => "histae_matches",
            Self::ContinuationRequested | Self::ConversationContinued => "histae_continuations",
            Self::PhotosRevealed => "histae_photos",
            Self::FirstPeriodEnding | Self::MatchExpiring => "histae_reminders",
            Self::NewSession | Self::DataRequestReplied | Self::BillingPaymentFailed => "histae_account",
        }
    }

    pub(super) const fn retention_seconds(self) -> i64 {
        let days = match self {
            Self::NewMatch
            | Self::NewMessage
            | Self::MatchExpiring
            | Self::BillingPaymentFailed => 90,
            Self::ContinuationRequested
            | Self::ConversationContinued
            | Self::PhotosRevealed
            | Self::FirstPeriodEnding
            | Self::DataRequestReplied
            | Self::NewSession => 1,
        };
        days * 24 * 60 * 60
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "new_match" => Some(Self::NewMatch),
            "new_message" => Some(Self::NewMessage),
            "match_expiring" => Some(Self::MatchExpiring),
            "billing_payment_failed" => Some(Self::BillingPaymentFailed),
            "continuation_requested" => Some(Self::ContinuationRequested),
            "conversation_continued" => Some(Self::ConversationContinued),
            "photos_revealed" => Some(Self::PhotosRevealed),
            "first_period_ending" => Some(Self::FirstPeriodEnding),
            "data_request_replied" => Some(Self::DataRequestReplied),
            "new_session" => Some(Self::NewSession),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewMatch => "new_match",
            Self::NewMessage => "new_message",
            Self::MatchExpiring => "match_expiring",
            Self::BillingPaymentFailed => "billing_payment_failed",
            Self::ContinuationRequested => "continuation_requested",
            Self::ConversationContinued => "conversation_continued",
            Self::PhotosRevealed => "photos_revealed",
            Self::FirstPeriodEnding => "first_period_ending",
            Self::DataRequestReplied => "data_request_replied",
            Self::NewSession => "new_session",
        }
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    #[test]
    fn new_types_round_trip_without_storing_display_copy_or_a_video_reward_type() {
        for kind in [
            NotificationType::ContinuationRequested,
            NotificationType::ConversationContinued,
            NotificationType::PhotosRevealed,
            NotificationType::FirstPeriodEnding,
            NotificationType::DataRequestReplied,
            NotificationType::NewSession,
        ] {
            assert_eq!(NotificationType::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(NotificationType::parse("advertising_credit_added"), None);
        let session_id = Uuid::new_v4();
        let login = NotificationIntent::NewSession { session_id };
        assert_eq!(login.excluded_session_id(), Some(session_id));
        let reveal = NotificationIntent::PhotosRevealed {
            match_id: Uuid::new_v4(),
            reveal_version: 3,
        };
        assert_eq!(reveal.payload()["reveal_version"], 3);
        assert_eq!(reveal.excluded_session_id(), None);
        let reply = NotificationIntent::DataRequestReplied {
            request_id: Uuid::new_v4(),
            response_status: "completed".to_owned(),
        };
        assert!(reply.payload().get("response").is_none());
        for payload in [login.payload(), reveal.payload(), reply.payload()] {
            assert!(payload.get("firstname").is_none());
            assert!(payload.get("content").is_none());
        }
    }
}
