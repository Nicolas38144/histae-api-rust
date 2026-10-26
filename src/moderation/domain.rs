use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profiles::domain::{ModerationReason, ModerationStatus};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationContentType {
    Photo,
    Bio,
    ProfileAnswer,
}

impl ModerationContentType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Bio => "bio",
            Self::ProfileAnswer => "profile_answer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "photo" => Some(Self::Photo),
            "bio" => Some(Self::Bio),
            "profile_answer" => Some(Self::ProfileAnswer),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationDecision {
    Approved,
    Rejected,
}

impl ModerationDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModerationCase {
    pub case_id: Uuid,
    pub user_id: Uuid,
    pub firstname: Option<String>,
    pub content_type: ModerationContentType,
    pub status: ModerationStatus,
    pub reason_codes: Vec<ModerationReason>,
    pub policy_version: String,
    pub version: i32,
    pub review_reason: Option<String>,
    pub reviewed_at: Option<String>,
    pub reviewed_by: Option<Uuid>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModerationDetail {
    #[serde(flatten)]
    pub case: ModerationCase,
    pub content: Option<String>,
    pub question: Option<String>,
    pub photo: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ModerationRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub firstname: Option<String>,
    pub content_type: ModerationContentType,
    pub status: ModerationStatus,
    pub reason_codes: Vec<ModerationReason>,
    pub policy_version: String,
    pub version: i32,
    pub review_reason: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub cursor_at: String,
    pub text_content: Option<String>,
    pub question: Option<String>,
    pub object_key: Option<String>,
}

impl ModerationRow {
    pub fn to_case(&self) -> ModerationCase {
        ModerationCase {
            case_id: self.id,
            user_id: self.user_id,
            firstname: self.firstname.clone(),
            content_type: self.content_type,
            status: self.status,
            reason_codes: self.reason_codes.clone(),
            policy_version: self.policy_version.clone(),
            version: self.version,
            review_reason: self.review_reason.clone(),
            reviewed_at: self.reviewed_at.map(wire_timestamp),
            reviewed_by: self.reviewed_by,
            created_at: wire_timestamp(self.created_at),
            updated_at: wire_timestamp(self.updated_at),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModerationReviewInput {
    pub version: i32,
    pub decision: ModerationDecision,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModerationReviewResult {
    Updated,
    NotFound,
    Stale,
    NotActionable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

pub fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}
