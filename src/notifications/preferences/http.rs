use axum::{Extension, Json, Router, http::StatusCode, routing::get};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::{
    ConversationMute, NotificationPreferenceRepository, NotificationPreferences, PreferenceChanges,
};
use crate::http::{
    error::ApiError,
    extract::{ApiDto, ValidatedJson, ValidatedPath},
    router::HttpState,
};
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};

pub fn routes(
    repository: NotificationPreferenceRepository,
    auth: MobileAuthState,
) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/users/me/notification-preferences",
            get(read).patch(update),
        )
        .route(
            "/api/matches/{id}/notification-preferences",
            get(read_mute).put(update_mute),
        )
        .layer(Extension(repository))
        .layer(Extension(auth))
}

impl ApiDto for PreferenceChanges {
    const ERROR_CODE: &'static str = "invalid_notification_preferences";
    const ERROR_MESSAGE: &'static str = "Supply at least one notification preference.";
    fn is_valid(&self) -> bool {
        !self.is_empty()
    }
}

#[derive(Deserialize)]
struct MatchPath {
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    id: Uuid,
}
impl ApiDto for MatchPath {
    const ERROR_CODE: &'static str = "invalid_match_id";
    const ERROR_MESSAGE: &'static str = "A valid match ID is required.";
    fn is_valid(&self) -> bool {
        crate::shared::validation::uuid_all(self.id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MuteBody {
    muted: bool,
    muted_until: Option<DateTime<Utc>>,
}
impl ApiDto for MuteBody {
    const ERROR_CODE: &'static str = "invalid_conversation_mute";
    const ERROR_MESSAGE: &'static str = "Choose no expiry or a future date within 31 days.";
    fn is_valid(&self) -> bool {
        match (self.muted, self.muted_until) {
            (false, Some(_)) => false,
            (_, None) => true,
            (true, Some(until)) => {
                let now = Utc::now();
                until > now && until <= now + Duration::days(31)
            }
        }
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "notification_settings_unavailable",
        "These notification settings are unavailable.",
    )
}

async fn read(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(repo): Extension<NotificationPreferenceRepository>,
) -> Result<Json<NotificationPreferences>, ApiError> {
    repo.get(identity.account.user_id)
        .await?
        .map(Json)
        .ok_or_else(unavailable)
}
async fn update(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(repo): Extension<NotificationPreferenceRepository>,
    ValidatedJson(changes): ValidatedJson<PreferenceChanges>,
) -> Result<Json<NotificationPreferences>, ApiError> {
    repo.update(identity.account.user_id, changes)
        .await?
        .map(Json)
        .ok_or_else(unavailable)
}
async fn read_mute(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(repo): Extension<NotificationPreferenceRepository>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
) -> Result<Json<ConversationMute>, ApiError> {
    repo.get_mute(identity.account.user_id, path.id)
        .await?
        .map(Json)
        .ok_or_else(unavailable)
}
async fn update_mute(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(repo): Extension<NotificationPreferenceRepository>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
    ValidatedJson(body): ValidatedJson<MuteBody>,
) -> Result<Json<ConversationMute>, ApiError> {
    repo.set_mute(
        identity.account.user_id,
        path.id,
        body.muted,
        body.muted_until,
    )
    .await?
    .map(Json)
    .ok_or_else(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_unknown_and_invalid_mute_changes() {
        assert!(!PreferenceChanges::default().is_valid());
        assert!(serde_json::from_str::<PreferenceChanges>(r#"{"quiet_hours":true}"#).is_err());
        assert!(serde_json::from_str::<PreferenceChanges>(r#"{"messages":"false"}"#).is_err());
        assert!(
            PreferenceChanges {
                messages: Some(false),
                ..Default::default()
            }
            .is_valid()
        );
        assert!(
            MuteBody {
                muted: true,
                muted_until: None
            }
            .is_valid()
        );
        assert!(
            MuteBody {
                muted: false,
                muted_until: None
            }
            .is_valid()
        );
        let later = Some(Utc::now() + Duration::hours(1));
        assert!(
            !MuteBody {
                muted: false,
                muted_until: later
            }
            .is_valid()
        );
        assert!(
            MuteBody {
                muted: true,
                muted_until: later
            }
            .is_valid()
        );
        assert!(
            !MuteBody {
                muted: true,
                muted_until: Some(Utc::now() - Duration::seconds(1))
            }
            .is_valid()
        );
        assert!(
            !MuteBody {
                muted: true,
                muted_until: Some(Utc::now() + Duration::days(32))
            }
            .is_valid()
        );
    }
}
