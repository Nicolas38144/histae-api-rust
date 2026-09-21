use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminRole {
    Admin,
    Superadmin,
}

impl AdminRole {
    pub fn parse(value: &str) -> Result<Self, AdminDomainError> {
        match value {
            "admin" => Ok(Self::Admin),
            "superadmin" => Ok(Self::Superadmin),
            _ => Err(AdminDomainError::InvalidStoredValue),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Superadmin => "superadmin",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChallengePurpose {
    BootstrapRegistration,
    AdditionalRegistration,
    Authentication,
}

impl ChallengePurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootstrapRegistration => "bootstrap_registration",
            Self::AdditionalRegistration => "additional_registration",
            Self::Authentication => "authentication",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BootstrapRow {
    pub id: Uuid,
    pub user_id: Uuid,
}

#[derive(Clone, Debug)]
pub struct ChallengeRow {
    pub ceremony_state: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct CredentialRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub role: AdminRole,
    pub credential_id: String,
    pub public_key: Vec<u8>,
    pub counter: u32,
    pub device_type: String,
    pub backed_up: bool,
    pub transports: Vec<String>,
    pub aaguid: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct ActiveSessionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub credential_id: Uuid,
    pub role: AdminRole,
    pub authenticated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct NewCredential {
    pub credential_id: String,
    pub public_key: Vec<u8>,
    pub counter: u32,
    pub device_type: String,
    pub backed_up: bool,
    pub transports: Vec<String>,
    pub aaguid: Option<Uuid>,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct NewSession {
    pub token_hash: [u8; 32],
    pub idle_expires_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialRevocation {
    Revoked,
    NotFound,
    LastCredential,
}

#[derive(Clone, Debug)]
pub struct SessionSummaryRow {
    pub id: Uuid,
    pub credential_id: Uuid,
    pub credential_name: String,
    pub authenticated_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct AuthEventRow {
    pub id: Uuid,
    pub event_type: String,
    pub credential_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminDomainError {
    InvalidCursor,
    InvalidStoredValue,
}

impl fmt::Display for AdminDomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidStoredValue => "invalid_stored_admin_value",
        })
    }
}

impl std::error::Error for AdminDomainError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    at: String,
    id: String,
}

pub fn decode_cursor(value: Option<&str>) -> Result<Option<EventCursor>, AdminDomainError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AdminDomainError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| AdminDomainError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at) {
        return Err(AdminDomainError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| AdminDomainError::InvalidCursor)?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(&cursor.id).map_err(|_| AdminDomainError::InvalidCursor)?;
    if !(1..=8).contains(&id.get_version_num()) || id.get_variant() != Variant::RFC4122 {
        return Err(AdminDomainError::InvalidCursor);
    }
    Ok(Some(EventCursor { at, id }))
}

pub fn encode_cursor(cursor: &EventCursor) -> Result<String, AdminDomainError> {
    let payload = CursorWire {
        at: wire_timestamp(cursor.at),
        id: cursor.id.hyphenated().to_string(),
    };
    serde_json::to_vec(&payload)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| AdminDomainError::InvalidCursor)
}

pub fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn valid_cursor_timestamp(value: &str) -> bool {
    let Some((date, fraction)) = value
        .strip_suffix('Z')
        .and_then(|value| value.rsplit_once('.'))
    else {
        return false;
    };
    (3..=6).contains(&fraction.len())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && date.len() == 19
        && DateTime::parse_from_rfc3339(value).is_ok()
}
