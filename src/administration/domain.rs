use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::matches::domain::{MatchRecord, MessageRecord};
use crate::profiles::domain::{ConsentType, LookingFor, Sex};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminUserRole {
    User,
    Admin,
    Superadmin,
}

impl AdminUserRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
            Self::Superadmin => "superadmin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "admin" => Some(Self::Admin),
            "superadmin" => Some(Self::Superadmin),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AdminUserStatus {
    Active,
    Banned,
}

impl AdminUserStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Banned => "banned",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub role: AdminUserRole,
    pub is_banned: bool,
    pub banned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub firstname: Option<String>,
    pub birthdate: Option<NaiveDate>,
    pub sex: Option<Sex>,
    pub photo_object_key: Option<String>,
    pub plan: String,
    pub onboarding_complete: bool,
    pub reports_received: i32,
    pub matches_count: i32,
    pub cursor_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminUser {
    pub user_id: Uuid,
    pub role: AdminUserRole,
    pub is_banned: bool,
    pub banned_at: Option<String>,
    pub created_at: String,
    pub firstname: Option<String>,
    pub birthdate: Option<NaiveDate>,
    pub sex: Option<Sex>,
    pub photo: Option<String>,
    pub plan: String,
    pub onboarding_complete: bool,
    pub reports_received: i32,
    pub matches_count: i32,
}

impl AdminUserRow {
    pub fn into_public(self, photo: Option<String>) -> AdminUser {
        AdminUser {
            user_id: self.id,
            role: self.role,
            is_banned: self.is_banned,
            banned_at: self.banned_at.map(wire_timestamp),
            created_at: wire_timestamp(self.created_at),
            firstname: self.firstname,
            birthdate: self.birthdate,
            sex: self.sex,
            photo,
            plan: self.plan,
            onboarding_complete: self.onboarding_complete,
            reports_received: self.reports_received,
            matches_count: self.matches_count,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPreferences {
    pub min_age: i32,
    pub max_age: i32,
    pub max_distance_km: i32,
    pub looking_for: LookingFor,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminTrait {
    pub id: Uuid,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminConsent {
    pub consent_type: ConsentType,
    pub granted: bool,
    pub document_version: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPresence {
    pub is_location_fresh: bool,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct AdminUserDetailRow {
    pub user: AdminUserRow,
    pub banned_reason: Option<String>,
    pub preferences: Option<AdminPreferences>,
    pub traits: Vec<AdminTrait>,
    pub consents: Vec<AdminConsent>,
    pub presence: Option<AdminPresence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminUserDetail {
    #[serde(flatten)]
    pub user: AdminUser,
    pub banned_reason: Option<String>,
    pub preferences: Option<AdminPreferences>,
    pub traits: Vec<AdminTrait>,
    pub consents: Vec<AdminConsent>,
    pub presence: Option<AdminPresence>,
}

#[derive(Clone, Debug)]
pub struct CursorMatchRow {
    pub item: MatchRecord,
    pub cursor_at: String,
}

#[derive(Clone, Debug)]
pub struct CursorMessageRow {
    pub item: MessageRecord,
    pub cursor_at: String,
}

#[derive(Clone, Debug)]
pub struct PageCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BanResult {
    Updated,
    NotFound,
    Forbidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoleChangeResult {
    Updated,
    Unchanged,
    NotFound,
    Forbidden,
}

pub fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}
