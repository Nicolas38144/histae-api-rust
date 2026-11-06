use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::SecretString,
    identity::{admin_role::AdminRole, mobile::otp::OtpService},
    infra::crypto::hmac_sha256,
};

pub const OBSERVATION_DAYS: i64 = 90;
pub const MAX_IMPORT_ENTRIES: usize = 200_000;
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

    pub fn accepts_duration(self, days: Option<u32>) -> bool {
        match self {
            Self::Automatic => days.is_none(),
            Self::Allow | Self::Block => days.is_some_and(|days| (1..=90).contains(&days)),
        }
    }

    pub const fn audit_action(self) -> &'static str {
        match self {
            Self::Automatic => "admin_reset_phone_policy",
            Self::Allow => "admin_allow_phone",
            Self::Block => "admin_block_phone",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Castle,
    Ip1,
}

impl Source {
    pub const ALL: [Self; 2] = [Self::Castle, Self::Ip1];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Castle => "castle",
            Self::Ip1 => "ip1",
        }
    }
}

pub struct ImportedNumber {
    pub key: PhoneKey,
    pub last_seen_at: DateTime<Utc>,
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
        let phone = canonical_phone(phone)?;
        Ok(Self {
            hash: hmac_sha256(&phone, key).map_err(|_| PolicyError::Unavailable)?,
            hint: format!("+33……{}", &phone[phone.len() - 4..]),
        })
    }
}

pub(super) fn canonical_phone(phone: &str) -> Result<String, PolicyError> {
    if phone.len() > 64 {
        return Err(PolicyError::Invalid);
    }
    OtpService::canonical_phone(phone).map_err(|_| PolicyError::Invalid)
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

impl Change {
    /// The repository supplies the active role read under the account lock.
    pub(super) fn authorized_reason(&self, role: AdminRole) -> Result<&str, PolicyError> {
        if role == AdminRole::Admin && self.decision != Decision::Allow {
            return Err(PolicyError::Forbidden);
        }
        if self.version < 0 || !self.decision.accepts_duration(self.days) {
            return Err(PolicyError::Invalid);
        }
        let supplied =
            crate::shared::text::javascript_trim(self.reason.as_deref().unwrap_or_default());
        let reason = role.audit_reason(supplied, "Superadmin phone policy change");
        if !(3..=500).contains(&reason.encode_utf16().count())
            || reason.chars().any(char::is_control)
        {
            return Err(PolicyError::Invalid);
        }
        Ok(reason)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_decisions_have_bounded_durations_and_automatic_has_none() {
        assert!(Decision::Automatic.accepts_duration(None));
        assert!(!Decision::Automatic.accepts_duration(Some(30)));
        for decision in [Decision::Allow, Decision::Block] {
            assert!(!decision.accepts_duration(None));
            for days in [0, 91, u32::MAX] {
                assert!(!decision.accepts_duration(Some(days)));
            }
            for days in [1, 30, 90] {
                assert!(decision.accepts_duration(Some(days)));
            }
        }
    }

    #[test]
    fn authorization_keeps_admin_reasons_and_superadmin_automatic_audits() {
        let mut change = Change {
            actor: Uuid::new_v4(),
            target: Target::Id(Uuid::new_v4()),
            decision: Decision::Allow,
            days: Some(30),
            version: 0,
            reason: None,
        };
        assert_eq!(
            change.authorized_reason(AdminRole::Admin),
            Err(PolicyError::Invalid)
        );
        assert_eq!(
            change.authorized_reason(AdminRole::Superadmin),
            Ok("Superadmin phone policy change")
        );
        change.reason = Some("  Verified reassignment  ".into());
        assert_eq!(
            change.authorized_reason(AdminRole::Admin),
            Ok("Verified reassignment")
        );
        change.decision = Decision::Block;
        assert_eq!(
            change.authorized_reason(AdminRole::Admin),
            Err(PolicyError::Forbidden)
        );
        assert_eq!(
            change.authorized_reason(AdminRole::Superadmin),
            Ok("Verified reassignment")
        );
        change.reason = Some("🙂".repeat(250));
        assert!(change.authorized_reason(AdminRole::Superadmin).is_ok());
        change.reason = Some("🙂".repeat(251));
        assert_eq!(
            change.authorized_reason(AdminRole::Superadmin),
            Err(PolicyError::Invalid)
        );
        change.reason = Some("A reason\nwith a control character".into());
        assert_eq!(
            change.authorized_reason(AdminRole::Superadmin),
            Err(PolicyError::Invalid)
        );
    }
}
