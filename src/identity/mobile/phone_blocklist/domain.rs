use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::SecretString, identity::mobile::otp::OtpService, infra::crypto::hmac_sha256};

pub const OBSERVATION_DAYS: i64 = 90;
pub const POLICY_LOCK: i64 = 86_302_016;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Automatic,
    Block,
    Allow,
}
impl Decision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Block => "block",
            Self::Allow => "allow",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Observation {
    pub source: String,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: Option<Uuid>,
    pub phone_hint: String,
    pub decision: Decision,
    pub decision_until: Option<DateTime<Utc>>,
    pub version: i64,
    pub blocked: bool,
    pub observations: Vec<Observation>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SourceStatus {
    pub source: String,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub imported_count: i32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Overview {
    pub entries: Vec<Entry>,
    pub total: i64,
    pub sources: Vec<SourceStatus>,
    pub ready: bool,
}

#[derive(Clone)]
pub struct PhoneKey {
    pub hash: String,
    pub hint: String,
}
impl PhoneKey {
    pub fn new(phone: &str, key: &SecretString) -> Result<Self, PolicyError> {
        if phone.len() > 64 {
            return Err(PolicyError::Invalid);
        }
        let phone = OtpService::canonical_phone(phone).map_err(|_| PolicyError::Invalid)?;
        Ok(Self {
            hash: hmac_sha256(&phone, key).map_err(|_| PolicyError::Unavailable)?,
            hint: format!("+33……{}", &phone[phone.len() - 4..]),
        })
    }
}

#[derive(Clone)]
pub enum Target {
    Id(Uuid),
    Phone(PhoneKey),
}
pub struct Change {
    pub actor: Uuid,
    pub target: Target,
    pub decision: Decision,
    pub days: Option<u32>,
    pub version: i64,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyError {
    Forbidden,
    Invalid,
    Conflict,
    NotFound,
    Unavailable,
    KeyMismatch,
}
impl From<crate::infra::postgres::DatabaseError> for PolicyError {
    fn from(_: crate::infra::postgres::DatabaseError) -> Self {
        Self::Unavailable
    }
}
impl PolicyError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::KeyMismatch => "phone_blocklist_key_mismatch",
            _ => "phone_policy_unavailable",
        }
    }
}
impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Forbidden => "admin_action_forbidden",
            Self::Invalid => "invalid_admin_request",
            Self::Conflict => "resource_changed",
            Self::NotFound => "resource_not_found",
            Self::Unavailable => "phone_policy_unavailable",
            Self::KeyMismatch => "phone_blocklist_key_mismatch",
        })
    }
}
impl std::error::Error for PolicyError {}
