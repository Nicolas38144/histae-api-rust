use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::BlockedUser;
use super::service::{PrivacyError, PrivacyService};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedPath};
use crate::http::router::HttpState;
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};

#[derive(Clone)]
pub struct PrivacyHttpState {
    service: PrivacyService,
}

impl PrivacyHttpState {
    pub fn new(service: PrivacyService) -> Self {
        Self { service }
    }
}

pub fn routes(state: PrivacyHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/blocks", get(list))
        .route("/api/users/me/blocks/{userId}", post(block).delete(unblock))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Serialize)]
struct BlocksResponse {
    blocks: Vec<BlockedUser>,
}

async fn list(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<PrivacyHttpState>,
) -> Result<Json<BlocksResponse>, ApiError> {
    state
        .service
        .blocked_users(identity.account.user_id)
        .await
        .map(|blocks| Json(BlocksResponse { blocks }))
        .map_err(privacy_error)
}

#[derive(Deserialize)]
struct UserPath {
    #[serde(rename = "userId")]
    user_id: String,
}

impl UserPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.user_id)
    }
}

impl ApiDto for UserPath {
    const ERROR_CODE: &'static str = "invalid_user_id";
    const ERROR_MESSAGE: &'static str = "The user ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

async fn block(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<PrivacyHttpState>,
    ValidatedPath(path): ValidatedPath<UserPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .block(
            identity.account.user_id,
            path.id().ok_or_else(invalid_user_id)?,
        )
        .await
        .map_err(privacy_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<PrivacyHttpState>,
    ValidatedPath(path): ValidatedPath<UserPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .unblock(
            identity.account.user_id,
            path.id().ok_or_else(invalid_user_id)?,
        )
        .await
        .map_err(privacy_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn privacy_error(error: PrivacyError) -> ApiError {
    match error {
        PrivacyError::InvalidBlock => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_block_request",
            "An account cannot block itself.",
        ),
        PrivacyError::UserNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "user_not_found",
            "The user to block was not found.",
        ),
        PrivacyError::Database(error) => error.into(),
    }
}

fn canonical_uuid(value: &str) -> Option<Uuid> {
    let parsed = Uuid::parse_str(value).ok()?;
    (value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
        && (1..=8).contains(&parsed.get_version_num())
        && parsed.get_variant() == Variant::RFC4122)
        .then_some(parsed)
}

fn invalid_user_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_user_id",
        "The user ID must be a valid UUID.",
    )
}
