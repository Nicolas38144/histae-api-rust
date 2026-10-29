use axum::{
    Json, Router,
    extract::{Extension, FromRequestParts},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    billing::web::{
        WebBillingError, WebBillingService,
        http::{error as auth_error, no_store, security},
    },
    config::{LimitPolicy, WebBillingConfig},
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson},
        lifecycle::ClientIp,
        rate_limit::RateLimiter,
        router::HttpState,
    },
    localization::http::Locale,
    media::http::photo_error,
    privacy::erasure::{
        AccountDeletionService, http::account_deletion_error, valid_deletion_token,
    },
    profiles::http::profile_error,
};

use super::service::{DataCategory, DataDeletionError, WebDataDeletionService};

#[derive(Clone)]
pub struct WebPrivacyState {
    pub auth: WebBillingService,
    pub config: WebBillingConfig,
    pub limiter: RateLimiter,
    pub policy: LimitPolicy,
    pub deletion: AccountDeletionService,
    pub data: WebDataDeletionService,
}

pub fn routes(state: WebPrivacyState) -> Router<HttpState> {
    Router::new()
        .route("/api/web/privacy/deletion-token", post(deletion_token))
        .route("/api/web/privacy/account", delete(delete_account))
        .route("/api/web/privacy/data", delete(delete_data))
        .layer(axum::middleware::from_fn(no_store))
        .layer(Extension(state))
}

// Check before DTO validation. The global transport middleware owns body
// size and JSON syntax checks; valid JSON with invalid fields still needs guards.
struct AuthenticatedWeb(Uuid);

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedWeb {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Extension(web) = Extension::<WebPrivacyState>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::internal())?;
        security::validate_website_origin(&web.config, &parts.headers)?;
        let token = security::token(&parts.headers, &web.config)
            .ok_or_else(|| auth_error(WebBillingError::Authentication))?;
        let identity = web.auth.identity(&token).await.map_err(auth_error)?;
        web.limiter
            .enforce(
                "web-privacy",
                &identity.user_id.to_string(),
                &web.policy,
                "privacy_rate_limit_exceeded",
            )
            .await?;
        Ok(Self(identity.user_id))
    }
}

async fn deletion_token(
    AuthenticatedWeb(user): AuthenticatedWeb,
    Extension(state): Extension<WebPrivacyState>,
) -> Result<Response, ApiError> {
    let issued = state
        .deletion
        .issue(user)
        .await
        .map_err(account_deletion_error)?;
    Ok((StatusCode::CREATED, Json(issued)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteAccountBody {
    confirmation_token: String,
}
impl ApiDto for DeleteAccountBody {
    fn is_valid(&self) -> bool {
        valid_deletion_token(&self.confirmation_token)
    }
}

async fn delete_account(
    AuthenticatedWeb(user): AuthenticatedWeb,
    Extension(state): Extension<WebPrivacyState>,
    ValidatedJson(body): ValidatedJson<DeleteAccountBody>,
) -> Result<Response, ApiError> {
    let accepted = state
        .deletion
        .accept(user, &body.confirmation_token)
        .await
        .map_err(account_deletion_error)?;
    // The erasure transaction deactivates the account, invalidating every web
    // session immediately. Expire this browser's cookie without another DB write.
    let mut response = (StatusCode::ACCEPTED, Json(accepted)).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        security::set_cookie(&state.config, "", 0)?,
    );
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteDataBody {
    category: DataCategory,
}
impl ApiDto for DeleteDataBody {}

async fn delete_data(
    AuthenticatedWeb(user): AuthenticatedWeb,
    Extension(state): Extension<WebPrivacyState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    locale: Locale,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<DeleteDataBody>,
) -> Result<StatusCode, ApiError> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    state
        .data
        .delete(
            user,
            body.category,
            ip.to_string(),
            user_agent,
            locale.anonymous().language,
        )
        .await
        .map_err(|error| match error {
            DataDeletionError::Photo(error) => photo_error(error),
            DataDeletionError::Profile(error) => profile_error(error),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_explicit_partial_categories_are_accepted_and_never_a_target_user() {
        for category in ["photo", "location", "sensitive_profile"] {
            assert!(serde_json::from_value::<DeleteDataBody>(json!({"category":category})).is_ok());
        }
        for body in [
            json!({"category":"account"}),
            json!({"category":"messages"}),
            json!({"category":"photo", "user_id":Uuid::new_v4()}),
            json!({}),
        ] {
            assert!(serde_json::from_value::<DeleteDataBody>(body).is_err());
        }
        assert!(
            !DeleteAccountBody {
                confirmation_token: "confirmed".into(),
            }
            .is_valid()
        );
    }
}
