use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::config::LegalConfig;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    Male,
    Female,
    Other,
}

impl Sex {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
            Self::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "male" => Some(Self::Male),
            "female" => Some(Self::Female),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LookingFor {
    Male,
    Female,
    Both,
    Other,
}

impl LookingFor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
            Self::Both => "both",
            Self::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "male" => Some(Self::Male),
            "female" => Some(Self::Female),
            "both" => Some(Self::Both),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentType {
    TermsOfServiceAcceptance,
    PrivacyNoticeAcknowledgement,
    SensitiveDataConsent,
    LocationConsent,
}

impl ConsentType {
    pub const ALL: [Self; 4] = [
        Self::TermsOfServiceAcceptance,
        Self::PrivacyNoticeAcknowledgement,
        Self::SensitiveDataConsent,
        Self::LocationConsent,
    ];
    pub const ONBOARDING: [Self; 2] = [
        Self::TermsOfServiceAcceptance,
        Self::PrivacyNoticeAcknowledgement,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TermsOfServiceAcceptance => "terms_of_service_acceptance",
            Self::PrivacyNoticeAcknowledgement => "privacy_notice_acknowledgement",
            Self::SensitiveDataConsent => "sensitive_data_consent",
            Self::LocationConsent => "location_consent",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "terms_of_service_acceptance" => Some(Self::TermsOfServiceAcceptance),
            "privacy_notice_acknowledgement" => Some(Self::PrivacyNoticeAcknowledgement),
            "sensitive_data_consent" => Some(Self::SensitiveDataConsent),
            "location_consent" => Some(Self::LocationConsent),
            _ => None,
        }
    }

    pub fn version(self, config: &LegalConfig) -> &str {
        match self {
            Self::TermsOfServiceAcceptance => &config.terms_version,
            Self::PrivacyNoticeAcknowledgement => &config.privacy_version,
            Self::SensitiveDataConsent => &config.sensitive_data_consent_version,
            Self::LocationConsent => &config.location_consent_version,
        }
    }

    pub fn document_url(self, config: &LegalConfig) -> String {
        match self {
            Self::TermsOfServiceAcceptance => config.terms_url.as_str(),
            Self::PrivacyNoticeAcknowledgement => config.privacy_url.as_str(),
            Self::SensitiveDataConsent => config.sensitive_data_consent_url.as_str(),
            Self::LocationConsent => config.location_consent_url.as_str(),
        }
        .to_owned()
    }

    pub const fn is_non_withdrawable(self) -> bool {
        matches!(
            self,
            Self::TermsOfServiceAcceptance | Self::PrivacyNoticeAcknowledgement
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationStatus {
    Pending,
    Approved,
    Rejected,
}

impl ModerationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationReason {
    Spam,
    Insult,
    PersonalContact,
    SexualContent,
    FaceNotDetected,
    MultipleFaces,
    Blurry,
    ExplicitImage,
    AnalysisUnavailable,
    LegacyUnreviewed,
}

impl ModerationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Insult => "insult",
            Self::PersonalContact => "personal_contact",
            Self::SexualContent => "sexual_content",
            Self::FaceNotDetected => "face_not_detected",
            Self::MultipleFaces => "multiple_faces",
            Self::Blurry => "blurry",
            Self::ExplicitImage => "explicit_image",
            Self::AnalysisUnavailable => "analysis_unavailable",
            Self::LegacyUnreviewed => "legacy_unreviewed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "spam" => Some(Self::Spam),
            "insult" => Some(Self::Insult),
            "personal_contact" => Some(Self::PersonalContact),
            "sexual_content" => Some(Self::SexualContent),
            "face_not_detected" => Some(Self::FaceNotDetected),
            "multiple_faces" => Some(Self::MultipleFaces),
            "blurry" => Some(Self::Blurry),
            "explicit_image" => Some(Self::ExplicitImage),
            "analysis_unavailable" => Some(Self::AnalysisUnavailable),
            "legacy_unreviewed" => Some(Self::LegacyUnreviewed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomatedModerationDecision {
    pub status: ModerationStatus,
    pub reasons: Vec<ModerationReason>,
    pub policy_version: &'static str,
}

#[derive(Clone, Debug)]
pub struct ProfileRecord {
    pub user_id: Uuid,
    pub firstname: String,
    pub birthdate: NaiveDate,
    pub sex: Option<Sex>,
    pub bio: Option<String>,
    pub photo_object_key: Option<String>,
    pub profile_answers: Vec<Value>,
    pub bio_moderation_status: Option<ModerationStatus>,
    pub bio_moderation_reasons: Vec<ModerationReason>,
    pub photo_moderation_status: Option<ModerationStatus>,
    pub photo_moderation_reasons: Vec<ModerationReason>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicProfile {
    pub user_id: Uuid,
    pub firstname: String,
    pub birthdate: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sex: Option<Sex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
    pub profile_answers: Vec<Value>,
    pub moderation: PublicModeration,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicModeration {
    pub bio: Option<PublicModerationState>,
    pub photo: Option<PublicModerationState>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicModerationState {
    pub status: ModerationStatus,
    pub reasons: Vec<ModerationReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preferences {
    pub user_id: Uuid,
    pub min_age: i32,
    pub max_age: i32,
    pub max_distance_km: i32,
    pub looking_for: LookingFor,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicPreferences {
    pub user_id: Uuid,
    pub min_age: i32,
    pub max_age: i32,
    pub max_distance_km: i32,
    pub looking_for: LookingFor,
}

impl From<Preferences> for PublicPreferences {
    fn from(value: Preferences) -> Self {
        Self {
            user_id: value.user_id,
            min_age: value.min_age,
            max_age: value.max_age,
            max_distance_km: value.max_distance_km,
            looking_for: value.looking_for,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProfileInput {
    pub firstname: String,
    pub birthdate: NaiveDate,
    pub sex: Option<Sex>,
    pub bio: Option<String>,
    pub bio_moderation: Option<AutomatedModerationDecision>,
}

#[derive(Clone, Copy, Debug)]
pub struct PreferencesInput {
    pub min_age: i32,
    pub max_age: i32,
    pub max_distance_km: i32,
    pub looking_for: LookingFor,
}

#[derive(Clone, Copy, Debug)]
pub struct PresenceInput {
    pub latitude: f64,
    pub longitude: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub struct ConsentChange {
    pub consent_type: ConsentType,
    pub granted: bool,
}

#[derive(Clone, Debug)]
pub struct VersionedConsentChange {
    pub consent_type: ConsentType,
    pub granted: bool,
    pub document_version: String,
}

#[derive(Clone, Debug)]
pub struct ConsentRecord {
    pub consent_type: ConsentType,
    pub granted: bool,
    pub document_version: String,
    pub granted_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConsentState {
    pub consents: Vec<PublicConsent>,
    pub onboarding_complete: bool,
    pub required_actions: Vec<ConsentType>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicConsent {
    pub consent_type: ConsentType,
    pub granted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_version: Option<String>,
    pub required_document_version: String,
    pub document_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Updated,
    AccountNotFound,
    RequiredConsentMissing,
}
