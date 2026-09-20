use axum::extract::{Extension, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant, Version};

use super::service::{AuthenticatedMobile as AuthenticatedIdentity, MobileAuthError};
use super::service::{MobileAuthService, MobileSessionPage, TokenPair};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::lifecycle::ClientIp;
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;

#[derive(Clone)]
pub struct MobileAuthState {
    service: MobileAuthService,
    limiter: RateLimiter,
    session_policy: LimitPolicy,
}

impl MobileAuthState {
    pub fn new(
        service: MobileAuthService,
        limiter: RateLimiter,
        session_policy: LimitPolicy,
    ) -> Self {
        Self {
            service,
            limiter,
            session_policy,
        }
    }
}

pub fn routes(state: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/auth/me", get(me))
        .route("/api/auth/refresh", post(refresh))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/sessions", get(sessions))
        .route("/api/auth/sessions/{id}", delete(revoke_session))
        .route("/api/auth/logout-all", post(logout_all))
        .layer(Extension(state))
}

#[derive(Clone, Debug)]
pub struct AuthenticatedMobile(pub AuthenticatedIdentity);

impl<S> FromRequestParts<S> for AuthenticatedMobile
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let bearer = bearer_token(parts)?;
        let Extension(auth_state) = Extension::<MobileAuthState>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::internal())?;
        auth_state
            .service
            .authenticate(&bearer)
            .await
            .map(Self)
            .map_err(api_error)
    }
}

#[derive(Clone, Debug)]
pub struct OnboardedMobile(pub AuthenticatedIdentity);

impl<S> FromRequestParts<S> for OnboardedMobile
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let AuthenticatedMobile(identity) =
            AuthenticatedMobile::from_request_parts(parts, state).await?;
        if !identity.account.onboarding_complete {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "onboarding_incomplete",
                "The current terms and privacy notice must be acknowledged before using this route.",
            ));
        }
        Ok(Self(identity))
    }
}

fn bearer_token(parts: &Parts) -> Result<String, ApiError> {
    let value = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let mut segments = value.split_whitespace();
    let scheme = segments.next();
    let token = segments.next();
    if !scheme.is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        || token.is_none()
        || segments.next().is_some()
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "A valid Bearer Authorization header is required.",
        ));
    }
    token.map(str::to_owned).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "A valid Bearer Authorization header is required.",
        )
    })
}

#[derive(Serialize)]
struct MeResponse {
    user_id: Uuid,
    onboarding_complete: bool,
}

async fn me(AuthenticatedMobile(identity): AuthenticatedMobile) -> Json<MeResponse> {
    Json(MeResponse {
        user_id: identity.account.user_id,
        onboarding_complete: identity.account.onboarding_complete,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshBody {
    refresh_token: String,
}

impl ApiDto for RefreshBody {
    fn is_valid(&self) -> bool {
        self.refresh_token.chars().count() <= 128
    }
}

async fn refresh(
    Extension(state): Extension<MobileAuthState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<RefreshBody>,
) -> Result<Json<TokenPair>, ApiError> {
    state
        .limiter
        .enforce(
            "refresh",
            &client_ip.to_string(),
            &state.session_policy,
            "refresh_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .refresh(&body.refresh_token)
        .await
        .map(Json)
        .map_err(api_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogoutBody {
    refresh_token: String,
    device_id: Option<Uuid>,
}

impl ApiDto for LogoutBody {
    fn is_valid(&self) -> bool {
        self.refresh_token.chars().count() <= 128 && self.device_id.is_none_or(valid_uuid_all)
    }
}

async fn logout(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<MobileAuthState>,
    ValidatedJson(body): ValidatedJson<LogoutBody>,
) -> Result<impl IntoResponse, ApiError> {
    session_rate_limit(&state, identity.account.user_id).await?;
    state
        .service
        .logout(
            identity.account.user_id,
            identity.session_id,
            &body.refresh_token,
            body.device_id,
        )
        .await
        .map_err(api_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionQuery {
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

impl ApiDto for SessionQuery {
    const ERROR_CODE: &'static str = "invalid_session_query";
    const ERROR_MESSAGE: &'static str = "The session query is invalid.";

    fn is_valid(&self) -> bool {
        (1..=100).contains(&self.limit)
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| value.chars().count() <= 512)
    }
}

const fn default_limit() -> u32 {
    20
}

async fn sessions(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<MobileAuthState>,
    ValidatedQuery(query): ValidatedQuery<SessionQuery>,
) -> Result<Json<MobileSessionPage>, ApiError> {
    session_rate_limit(&state, identity.account.user_id).await?;
    state
        .service
        .list_sessions(
            identity.account.user_id,
            identity.session_id,
            query.limit,
            query.cursor.as_deref(),
        )
        .await
        .map(Json)
        .map_err(api_error)
}

#[derive(Deserialize)]
struct SessionPath {
    id: Uuid,
}

impl ApiDto for SessionPath {
    const ERROR_CODE: &'static str = "invalid_session_id";
    const ERROR_MESSAGE: &'static str = "The session ID must be a UUID v4.";

    fn is_valid(&self) -> bool {
        self.id.get_version() == Some(Version::Random) && self.id.get_variant() == Variant::RFC4122
    }
}

async fn revoke_session(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<MobileAuthState>,
    ValidatedPath(path): ValidatedPath<SessionPath>,
) -> Result<impl IntoResponse, ApiError> {
    session_rate_limit(&state, identity.account.user_id).await?;
    state
        .service
        .revoke_session(identity.account.user_id, identity.session_id, path.id)
        .await
        .map_err(api_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogoutAllBody {
    confirm: bool,
}

impl ApiDto for LogoutAllBody {
    fn is_valid(&self) -> bool {
        self.confirm
    }
}

#[derive(Serialize)]
struct LogoutAllResponse {
    revoked_sessions: u64,
}

async fn logout_all(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<MobileAuthState>,
    ValidatedJson(_body): ValidatedJson<LogoutAllBody>,
) -> Result<Json<LogoutAllResponse>, ApiError> {
    session_rate_limit(&state, identity.account.user_id).await?;
    let revoked_sessions = state
        .service
        .logout_all(identity.account.user_id, identity.session_id)
        .await
        .map_err(api_error)?;
    Ok(Json(LogoutAllResponse { revoked_sessions }))
}

async fn session_rate_limit(state: &MobileAuthState, user_id: Uuid) -> Result<(), ApiError> {
    state
        .limiter
        .enforce(
            "mobile-sessions",
            &user_id.hyphenated().to_string(),
            &state.session_policy,
            "session_rate_limit_exceeded",
        )
        .await
}

fn valid_uuid_all(value: Uuid) -> bool {
    (1..=8).contains(&value.get_version_num()) && value.get_variant() == Variant::RFC4122
}

fn api_error(error: MobileAuthError) -> ApiError {
    match error {
        MobileAuthError::InvalidRefreshToken => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_or_expired_refresh_token",
            "The refresh token is invalid, expired, or already used.",
        ),
        MobileAuthError::InvalidAccessToken => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_or_expired_access_token",
            "The access token is invalid or expired.",
        ),
        MobileAuthError::AuthenticationRequired => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "A valid access token is required.",
        ),
        MobileAuthError::AccountUnavailable => ApiError::new(
            StatusCode::FORBIDDEN,
            "account_unavailable",
            "This account is unavailable.",
        ),
        MobileAuthError::AccountCheckFailed => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "account_check_failed",
            "The account could not be verified.",
        ),
        MobileAuthError::SessionNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "The mobile session could not be found.",
        ),
        MobileAuthError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        MobileAuthError::Internal => ApiError::internal(),
    }
}
