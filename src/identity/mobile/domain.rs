use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountRole {
    User,
    Admin,
    Superadmin,
}

impl AccountRole {
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "user" => Ok(Self::User),
            "admin" => Ok(Self::Admin),
            "superadmin" => Ok(Self::Superadmin),
            _ => Err(DomainError::InvalidStoredRole),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
            Self::Superadmin => "superadmin",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveAccount {
    pub user_id: Uuid,
    pub role: AccountRole,
    pub is_banned: bool,
    pub onboarding_complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MobileSessionIdentity {
    pub user_id: Uuid,
    pub session_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MobileSessionRow {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub cursor_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCursor {
    pub at: String,
    pub id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RotationOutcome {
    Rotated(MobileSessionIdentity),
    Invalid,
    ReplayRevoked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidCursor,
    InvalidStoredRole,
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidStoredRole => "invalid_stored_role",
        })
    }
}

impl std::error::Error for DomainError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    at: String,
    id: String,
}

pub fn decode_cursor(value: Option<&str>) -> Result<Option<SessionCursor>, DomainError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| DomainError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| DomainError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at) {
        return Err(DomainError::InvalidCursor);
    }
    let id = Uuid::parse_str(&cursor.id).map_err(|_| DomainError::InvalidCursor)?;
    let version = id.get_version_num();
    if !(1..=8).contains(&version) || id.get_variant() != Variant::RFC4122 {
        return Err(DomainError::InvalidCursor);
    }
    Ok(Some(SessionCursor { at: cursor.at, id }))
}

pub fn encode_cursor(cursor: &SessionCursor) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(&CursorWire {
        at: cursor.at.clone(),
        id: cursor.id.hyphenated().to_string(),
    })
    .map_err(|_| DomainError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
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
    if fraction.len() < 3
        || fraction.len() > 6
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || date.len() != 19
    {
        return false;
    }
    DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_matches_the_nest_base64url_shape_and_timestamp_rules() {
        let cursor = SessionCursor {
            at: "2026-09-20T12:34:56.123456Z".to_owned(),
            id: Uuid::new_v4(),
        };
        let encoded = encode_cursor(&cursor).expect("encode cursor");
        assert_eq!(decode_cursor(Some(&encoded)), Ok(Some(cursor)));
        assert_eq!(
            decode_cursor(Some("eyJhdCI6ImJhZCIsImlkIjoibm90LWEtdXVpZCJ9")),
            Err(DomainError::InvalidCursor)
        );
    }
}
