use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profiles::domain::Sex;

pub const SWIPE_RETENTION_DAYS: i64 = 365;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwipeDecision {
    Like,
    Pass,
}

impl SwipeDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Pass => "pass",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "like" => Some(Self::Like),
            "pass" => Some(Self::Pass),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscoveryCursor {
    pub distance_km: f64,
    pub id: Uuid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FeedProfileAnswer {
    pub question_id: Uuid,
    pub code: String,
    pub question: String,
    pub answer: String,
    pub position: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryCandidateRow {
    pub user_id: Uuid,
    pub firstname: String,
    pub age: i32,
    pub sex: Sex,
    pub bio: Option<String>,
    pub distance_km: f64,
    pub traits: Vec<String>,
    pub profile_answers: Vec<FeedProfileAnswer>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryBatch {
    pub ready: bool,
    pub candidates: Vec<DiscoveryCandidateRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeedCandidate {
    pub user_id: Uuid,
    pub firstname: String,
    pub age: i32,
    pub sex: Sex,
    pub bio: Option<String>,
    pub distance_km: f64,
    pub traits: Vec<String>,
    pub profile_answers: Vec<FeedProfileAnswer>,
}

impl From<DiscoveryCandidateRow> for FeedCandidate {
    fn from(value: DiscoveryCandidateRow) -> Self {
        Self {
            user_id: value.user_id,
            firstname: value.firstname,
            age: value.age,
            sex: value.sex,
            bio: value.bio,
            // Keep the numeric contract without publishing sub-kilometre ranges.
            // Stored locations are coarse cell centres, including for filtering
            // and ordering; rounding alone would not prevent repeated probing.
            distance_km: (value.distance_km / 5.0).ceil().max(1.0) * 5.0,
            traits: value.traits,
            profile_answers: value.profile_answers,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryRequiredAction {
    Profile,
    Sex,
    Photo,
    Preferences,
    SensitiveDataConsent,
    LocationConsent,
    FreshPresence,
    ResumeDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryStatusRow {
    pub paused: bool,
    pub has_profile: bool,
    pub has_sex: bool,
    pub has_approved_photo: bool,
    pub has_preferences: bool,
    pub has_sensitive_consent: bool,
    pub has_location_consent: bool,
    pub has_fresh_presence: bool,
    pub presence_expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoverySettings {
    pub paused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryStatus {
    pub ready: bool,
    pub required_actions: Vec<DiscoveryRequiredAction>,
    pub presence_expires_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwipeRecord {
    pub actor_id: Uuid,
    pub target_id: Uuid,
    pub decision: SwipeDecision,
    pub swiped_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordedSwipe {
    pub created: bool,
    pub decision: SwipeDecision,
}
