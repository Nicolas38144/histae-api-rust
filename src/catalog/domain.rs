use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::profiles::domain::{AutomatedModerationDecision, ModerationReason, ModerationStatus};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanFeature {
    pub code: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub feature_value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubscriptionPlan {
    pub code: String,
    pub display_name: String,
    pub monthly_price_cents: i32,
    pub annual_price_cents: i32,
    pub currency: String,
    pub trial_days: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly_continuation_limit: Option<i32>,
    pub features: Vec<PlanFeature>,
}

#[derive(Clone, Debug)]
pub struct PlanRow {
    pub code: String,
    pub display_name: String,
    pub monthly_price_cents: i32,
    pub annual_price_cents: i32,
    pub currency: String,
    pub weekly_continuation_limit: Option<i32>,
    pub trial_days: i32,
    pub feature_code: Option<String>,
    pub feature_name: Option<String>,
    pub feature_description: Option<String>,
    pub feature_value: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Trait {
    pub id: Uuid,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileQuestionCategory {
    DailyLife,
    Personality,
    Interests,
    Relationships,
    Conversation,
}

impl ProfileQuestionCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DailyLife => "daily_life",
            Self::Personality => "personality",
            Self::Interests => "interests",
            Self::Relationships => "relationships",
            Self::Conversation => "conversation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "daily_life" => Some(Self::DailyLife),
            "personality" => Some(Self::Personality),
            "interests" => Some(Self::Interests),
            "relationships" => Some(Self::Relationships),
            "conversation" => Some(Self::Conversation),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileQuestion {
    pub id: Uuid,
    pub code: String,
    pub prompt: String,
    pub category: ProfileQuestionCategory,
    pub display_order: i32,
}

#[derive(Clone, Debug)]
pub struct AdminProfileQuestionRow {
    pub question: ProfileQuestion,
    pub answer_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdminProfileQuestion {
    pub id: Uuid,
    pub code: String,
    pub prompt: String,
    pub category: ProfileQuestionCategory,
    pub display_order: i32,
    pub answer_count: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl From<AdminProfileQuestionRow> for AdminProfileQuestion {
    fn from(value: AdminProfileQuestionRow) -> Self {
        Self {
            id: value.question.id,
            code: value.question.code,
            prompt: value.question.prompt,
            category: value.question.category,
            display_order: value.question.display_order,
            answer_count: value.answer_count,
            created_at: value
                .created_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            updated_at: value
                .updated_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileAnswer {
    pub question_id: Uuid,
    pub code: String,
    pub question: String,
    pub answer: String,
    pub position: i32,
    pub moderation_status: ModerationStatus,
    pub moderation_reasons: Vec<ModerationReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileAnswerInput {
    pub question_id: Uuid,
    pub answer: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedProfileAnswer {
    pub question_id: Uuid,
    pub answer: String,
    pub moderation: AutomatedModerationDecision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileQuestionInput {
    pub prompt: String,
    pub category: ProfileQuestionCategory,
    pub display_order: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileQuestionPatch {
    pub prompt: Option<String>,
    pub category: Option<ProfileQuestionCategory>,
    pub display_order: Option<i32>,
    pub supplied: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceAnswersOutcome {
    Updated,
    ProfileNotFound,
    QuestionNotFound,
}
