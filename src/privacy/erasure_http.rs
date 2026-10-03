use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::erasure::{
    AcceptedErasure, AccountDeletionError, AccountDeletionService, IssuedDeletionToken,
    valid_deletion_token,
};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson};
use crate::http::router::HttpState;
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState};

#[derive(Clone)]
pub struct AccountDeletionHttpState {
    service: AccountDeletionService,
}

impl AccountDeletionHttpState {
    pub fn new(service: AccountDeletionService) -> Self {
        Self { service }
    }
}

pub fn routes(state: AccountDeletionHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/deletion-token", post(issue_deletion_token))
        .route("/api/users/me", delete(accept_erasure))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn issue_deletion_token(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<AccountDeletionHttpState>,
) -> Result<(StatusCode, Json<IssuedDeletionToken>), ApiError> {
    state
        .service
        .issue(identity.account.user_id)
        .await
        .map(|token| (StatusCode::CREATED, Json(token)))
        .map_err(account_deletion_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmAccountDeletionBody {
    confirmation_token: String,
}

impl ApiDto for ConfirmAccountDeletionBody {
    const ERROR_CODE: &'static str = "invalid_account_deletion_payload";
    const ERROR_MESSAGE: &'static str = "The account deletion request body is invalid.";

    fn is_valid(&self) -> bool {
        valid_deletion_token(&self.confirmation_token)
    }
}

async fn accept_erasure(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<AccountDeletionHttpState>,
    ValidatedJson(body): ValidatedJson<ConfirmAccountDeletionBody>,
) -> Result<(StatusCode, Json<AcceptedErasure>), ApiError> {
    state
        .service
        .accept(identity.account.user_id, &body.confirmation_token)
        .await
        .map(|accepted| (StatusCode::ACCEPTED, Json(accepted)))
        .map_err(account_deletion_error)
}

fn account_deletion_error(error: AccountDeletionError) -> ApiError {
    match error {
        AccountDeletionError::AccountNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "account_not_found",
            "The account could not be found or has been deleted.",
        ),
        AccountDeletionError::InvalidOrExpiredToken => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_or_expired_deletion_token",
            "The account deletion confirmation token is invalid or expired.",
        ),
        AccountDeletionError::RandomUnavailable | AccountDeletionError::TimeOutOfRange => {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "account_deletion_unavailable",
                "The account deletion operation is temporarily unavailable.",
            )
        }
        AccountDeletionError::Database(error) => error.into(),
    }
}
