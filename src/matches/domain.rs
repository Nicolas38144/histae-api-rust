use chrono::{DateTime, Datelike as _, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::identity::mobile::domain::wire_timestamp;
use crate::profiles::domain::Sex;

pub const MATCH_WINDOW_HOURS: i64 = 24;
pub const MATCH_PURGE_DAYS: i64 = 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    AwaitingContinuation,
    Active,
    Confirmed,
    Expired,
    Ended,
}

impl MatchStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingContinuation => "awaiting_continuation",
            Self::Active => "active",
            Self::Confirmed => "confirmed",
            Self::Expired => "expired",
            Self::Ended => "ended",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "awaiting_continuation" => Some(Self::AwaitingContinuation),
            "active" => Some(Self::Active),
            "confirmed" => Some(Self::Confirmed),
            "expired" => Some(Self::Expired),
            "ended" => Some(Self::Ended),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchRecord {
    pub id: Uuid,
    pub user1_id: Uuid,
    pub user2_id: Uuid,
    pub status: MatchStatus,
    pub expires_at: DateTime<Utc>,
    pub purge_after: Option<DateTime<Utc>>,
    pub continuation_initiator_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserMatchRow {
    pub record: MatchRecord,
    pub cursor_at: String,
    pub other_user_id: Uuid,
    pub other_firstname: String,
    pub other_age: i32,
    pub other_sex: Option<Sex>,
    pub other_bio: Option<String>,
    pub other_photo: Option<String>,
    pub other_traits: Vec<String>,
    pub other_profile_answers: Vec<Value>,
    pub my_revealed: bool,
    pub photos_revealed: bool,
    pub my_continued: bool,
    pub unread_count: i32,
    pub last_message: Option<LastMessageRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LastMessageRow {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicMatch {
    pub id: Uuid,
    pub user1_id: Uuid,
    pub user2_id: Uuid,
    pub status: MatchStatus,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<String>,
}

impl From<MatchRecord> for PublicMatch {
    fn from(value: MatchRecord) -> Self {
        Self {
            id: value.id,
            user1_id: value.user1_id,
            user2_id: value.user2_id,
            status: value.status,
            expires_at: wire_timestamp(value.expires_at),
            purge_after: value.purge_after.map(wire_timestamp),
            created_at: wire_timestamp(value.created_at),
            last_message_at: value.last_message_at.map(wire_timestamp),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicUserMatch {
    pub id: Uuid,
    pub status: MatchStatus,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<String>,
    pub other_user: PublicMatchUser,
    pub my_revealed: bool,
    pub photos_revealed: bool,
    pub my_continued: bool,
    pub unread_count: i32,
    pub last_message: Option<PublicLastMessage>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicMatchUser {
    pub user_id: Uuid,
    pub firstname: String,
    pub age: i32,
    pub sex: Option<Sex>,
    pub bio: Option<String>,
    pub traits: Vec<String>,
    pub photo: Option<String>,
    pub profile_answers: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicLastMessage {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub content: String,
    pub created_at: String,
    pub read_at: Option<String>,
}

impl UserMatchRow {
    pub fn into_public(self, photo: Option<String>) -> PublicUserMatch {
        let record = self.record;
        PublicUserMatch {
            id: record.id,
            status: record.status,
            expires_at: wire_timestamp(record.expires_at),
            purge_after: record.purge_after.map(wire_timestamp),
            created_at: wire_timestamp(record.created_at),
            last_message_at: record.last_message_at.map(wire_timestamp),
            other_user: PublicMatchUser {
                user_id: self.other_user_id,
                firstname: self.other_firstname,
                age: self.other_age,
                sex: self.other_sex,
                bio: self.other_bio,
                traits: self.other_traits,
                photo,
                profile_answers: self.other_profile_answers,
            },
            my_revealed: self.my_revealed,
            photos_revealed: self.photos_revealed,
            my_continued: self.my_continued,
            unread_count: self.unread_count,
            last_message: self.last_message.map(|message| PublicLastMessage {
                id: message.id,
                sender_id: message.sender_id,
                content: message.content,
                created_at: wire_timestamp(message.created_at),
                read_at: message.read_at.map(wire_timestamp),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectivePlan {
    pub plan: String,
    pub weekly_limit: Option<i16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContinuationQuota {
    pub plan: String,
    pub used: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchAvailabilityFailure {
    NotFound,
    InvalidState,
    Expired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MatchCommandResult<T> {
    Available(T),
    Unavailable(MatchAvailabilityFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuationResult {
    Pending,
    Confirmed,
    AlreadyRecorded,
    NotAvailableYet,
    NotFound,
    InvalidState,
    Expired,
    QuotaReached,
}

pub fn start_of_utc_week(now: DateTime<Utc>) -> NaiveDate {
    let date = now.date_naive();
    date - chrono::TimeDelta::days(i64::from(date.weekday().num_days_from_monday()))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Weekday};

    use super::*;

    #[test]
    fn week_starts_on_monday_in_utc() {
        let sunday = Utc
            .with_ymd_and_hms(2030, 1, 6, 23, 59, 59)
            .single()
            .unwrap_or_else(|| unreachable!());
        let start = start_of_utc_week(sunday);
        assert_eq!(start.weekday(), Weekday::Mon);
        assert_eq!(start.to_string(), "2029-12-31");
    }
}
