use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    InappropriateContent,
    FakeProfile,
    Harassment,
    Spam,
    Other,
}

impl ReportReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InappropriateContent => "inappropriate_content",
            Self::FakeProfile => "fake_profile",
            Self::Harassment => "harassment",
            Self::Spam => "spam",
            Self::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "inappropriate_content" => Some(Self::InappropriateContent),
            "fake_profile" => Some(Self::FakeProfile),
            "harassment" => Some(Self::Harassment),
            "spam" => Some(Self::Spam),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Pending,
    Reviewed,
    Dismissed,
}

impl ReportStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reviewed => "reviewed",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "reviewed" => Some(Self::Reviewed),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReportRecord {
    pub id: Uuid,
    pub reporter_id: Uuid,
    pub reported_id: Uuid,
    pub match_id: Option<Uuid>,
    pub reason: ReportReason,
    pub description: Option<String>,
    pub status: ReportStatus,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct CursorReportRow {
    pub report: ReportRecord,
    pub cursor_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicReport {
    pub id: Uuid,
    pub reporter_id: Uuid,
    pub reported_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_id: Option<Uuid>,
    pub reason: ReportReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: ReportStatus,
    pub created_at: String,
}

impl From<ReportRecord> for PublicReport {
    fn from(report: ReportRecord) -> Self {
        Self {
            id: report.id,
            reporter_id: report.reporter_id,
            reported_id: report.reported_id,
            match_id: report.match_id,
            reason: report.reason,
            description: report.description,
            status: report.status,
            created_at: report
                .created_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PageCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}
