use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::domain::{ConsentChange, ConsentState, LookingFor, Sex};
use super::service::{ProfileError, ProfileService};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson};
use crate::http::lifecycle::ClientIp;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState, OnboardedMobile};

#[derive(Clone)]
pub struct ProfileHttpState {
    service: ProfileService,
}

impl ProfileHttpState {
    pub fn new(service: ProfileService) -> Self {
        Self { service }
    }
}

pub fn routes(state: ProfileHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me", get(get_profile))
        .route(
            "/api/users/me/consents",
            get(get_consents).put(update_consents),
        )
        .route("/api/users/me/profile", patch(update_profile))
        .route(
            "/api/users/me/preferences",
            get(get_preferences).patch(update_preferences),
        )
        .route("/api/users/me/presence", patch(update_presence))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn get_profile(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ProfileHttpState>,
) -> Result<Json<super::domain::PublicProfile>, ApiError> {
    state
        .service
        .profile(identity.account.user_id)
        .await
        .map(Json)
        .map_err(profile_error)
}

async fn get_consents(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<ProfileHttpState>,
) -> Result<Json<ConsentState>, ApiError> {
    state
        .service
        .consents(identity.account.user_id)
        .await
        .map(Json)
        .map_err(profile_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateConsentsBody {
    consents: Vec<ConsentChange>,
}

impl ApiDto for UpdateConsentsBody {
    const ERROR_CODE: &'static str = "invalid_consent_payload";
    const ERROR_MESSAGE: &'static str = "The consent request body is invalid.";

    fn is_valid(&self) -> bool {
        !self.consents.is_empty()
    }
}

async fn update_consents(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<ProfileHttpState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<UpdateConsentsBody>,
) -> Result<Json<ConsentState>, ApiError> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    state
        .service
        .update_consents(
            identity.account.user_id,
            body.consents,
            client_ip.to_string(),
            user_agent,
        )
        .await
        .map(Json)
        .map_err(profile_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProfileBody {
    firstname: String,
    birthdate: String,
    sex: Option<Sex>,
    bio: Option<String>,
}

impl ApiDto for UpdateProfileBody {
    const ERROR_CODE: &'static str = "invalid_profile_payload";
    const ERROR_MESSAGE: &'static str = "The profile request body is invalid.";

    fn is_valid(&self) -> bool {
        let bytes = self.birthdate.as_bytes();
        bytes.len() == 10
            && bytes.get(4) == Some(&b'-')
            && bytes.get(7) == Some(&b'-')
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    }
}

#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

async fn update_profile(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ProfileHttpState>,
    ValidatedJson(body): ValidatedJson<UpdateProfileBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_profile(
            identity.account.user_id,
            body.firstname,
            body.birthdate,
            body.sex,
            body.bio,
        )
        .await
        .map_err(profile_error)?;
    Ok(Json(MessageResponse {
        message: "profile updated",
    }))
}

async fn get_preferences(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ProfileHttpState>,
) -> Result<Json<super::domain::PublicPreferences>, ApiError> {
    state
        .service
        .preferences(identity.account.user_id)
        .await
        .map(Json)
        .map_err(profile_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePreferencesBody {
    min_age: f64,
    max_age: f64,
    max_distance_km: f64,
    looking_for: LookingFor,
}

impl ApiDto for UpdatePreferencesBody {
    const ERROR_CODE: &'static str = "invalid_preferences_payload";
    const ERROR_MESSAGE: &'static str = "The preferences request body is invalid.";
}

async fn update_preferences(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ProfileHttpState>,
    ValidatedJson(body): ValidatedJson<UpdatePreferencesBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_preferences(
            identity.account.user_id,
            body.min_age,
            body.max_age,
            body.max_distance_km,
            body.looking_for,
        )
        .await
        .map_err(profile_error)?;
    Ok(Json(MessageResponse {
        message: "preferences updated",
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePresenceBody {
    latitude: f64,
    longitude: f64,
}

impl ApiDto for UpdatePresenceBody {
    const ERROR_CODE: &'static str = "invalid_presence_payload";
    const ERROR_MESSAGE: &'static str = "The location request body is invalid.";
}

async fn update_presence(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ProfileHttpState>,
    ValidatedJson(body): ValidatedJson<UpdatePresenceBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_presence(identity.account.user_id, body.latitude, body.longitude)
        .await
        .map_err(profile_error)?;
    Ok(Json(MessageResponse {
        message: "presence updated",
    }))
}

fn profile_error(error: ProfileError) -> ApiError {
    match error {
        ProfileError::InvalidProfile => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_profile",
            "The profile does not meet the required constraints.",
        ),
        ProfileError::ProfileNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "profile_not_found",
            "The account exists, but its profile has not been completed yet.",
        ),
        ProfileError::InvalidPreferences => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_preferences",
            "The preferences do not meet the required constraints.",
        ),
        ProfileError::PreferencesNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "preferences_not_found",
            "The account preferences could not be found.",
        ),
        ProfileError::InvalidPresence => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_presence",
            "The location does not contain valid coordinates.",
        ),
        ProfileError::InvalidConsentPayload => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_consent_payload",
            "The consent request body is invalid.",
        ),
        ProfileError::RequiredConsentMissing => ApiError::new(
            StatusCode::FORBIDDEN,
            "required_consent_missing",
            "The required consent has not been granted.",
        ),
        ProfileError::AccountNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "account_not_found",
            "The account could not be found or has been deleted.",
        ),
        ProfileError::Database(error) => error.into(),
        ProfileError::PhotoUrlUnavailable => ApiError::internal(),
    }
}
