mod dto;
pub(crate) mod security;
use super::{ChallengeView, WebAccountError, WebAccountService};
use crate::{
    config::{LimitPolicy, WebAccountConfig},
    http::{
        error::ApiError, extract::ValidatedJson, lifecycle::ClientIp, rate_limit::RateLimiter,
        router::HttpState,
    },
    localization::http::Locale,
};
use axum::{
    Json, Router,
    extract::Extension,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use dto::{SendBody, VerifyBody};
use security::{set_cookie, token, validate_website_origin};
use uuid::Uuid;

#[derive(Clone)]
pub struct WebAccountState {
    pub service: WebAccountService,
    pub config: WebAccountConfig,
    pub limiter: RateLimiter,
    pub otp_policy: LimitPolicy,
}
pub fn routes(state: WebAccountState) -> Router<HttpState> {
    Router::new()
        .route("/api/web/account/otp/send", post(send))
        .route("/api/web/account/otp/verify", post(verify))
        .route(
            "/api/web/account/session",
            get(account_session).delete(logout),
        )
        .layer(axum::middleware::from_fn(no_store))
        .layer(Extension(state))
}
pub(crate) async fn no_store(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
pub(crate) fn error(error: WebAccountError) -> ApiError {
    let (status, code, message) = match error {
        WebAccountError::InvalidRequest => return ApiError::invalid_body(),
        WebAccountError::Authentication => (
            StatusCode::UNAUTHORIZED,
            "web_authentication_failed",
            "The account or verification code is invalid or expired.",
        ),
        WebAccountError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "idempotency_key_conflict",
            "Use the original parameters for this request.",
        ),
        WebAccountError::Internal => return ApiError::internal(),
        WebAccountError::Database(error) => return error.into(),
    };
    ApiError::new(status, code, message)
}
fn session_token(state: &WebAccountState, headers: &HeaderMap) -> Result<String, ApiError> {
    token(headers, &state.config).ok_or_else(|| error(WebAccountError::Authentication))
}
async fn send(
    Extension(state): Extension<WebAccountState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<SendBody>,
) -> Result<(StatusCode, Json<ChallengeView>), ApiError> {
    validate_website_origin(&state.config, &headers)?;
    let phone_key = state.service.phone_key(&body.phone_number).map_err(error)?;
    state
        .limiter
        .enforce(
            "otp-web-send-ip",
            &ip.to_string(),
            &state.otp_policy,
            "otp_rate_limit_exceeded",
        )
        .await?;
    state
        .limiter
        .enforce(
            "otp-web-send-phone",
            &phone_key,
            &state.otp_policy,
            "otp_rate_limit_exceeded",
        )
        .await?;
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|id| id.get_version_num() == 4)
        .ok_or_else(ApiError::invalid_body)?;
    state
        .service
        .start(
            key,
            body.user_id,
            &body.phone_number,
            locale.anonymous().language,
        )
        .await
        .map(|view| (StatusCode::ACCEPTED, Json(view)))
        .map_err(error)
}
async fn verify(
    Extension(state): Extension<WebAccountState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<VerifyBody>,
) -> Result<Response, ApiError> {
    validate_website_origin(&state.config, &headers)?;
    state
        .limiter
        .enforce(
            "otp-web-verify-ip",
            &ip.to_string(),
            &state.otp_policy,
            "otp_rate_limit_exceeded",
        )
        .await?;
    let token = state
        .service
        .verify(body.challenge_id, &body.phone_number, &body.otp)
        .await
        .map_err(error)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(&state.config, token.expose_secret(), 1800)?,
    );
    Ok(response)
}
async fn account_session(
    Extension(state): Extension<WebAccountState>,
    headers: HeaderMap,
) -> Result<Json<super::WebIdentity>, ApiError> {
    state
        .service
        .identity(&session_token(&state, &headers)?)
        .await
        .map(Json)
        .map_err(error)
}
async fn logout(
    Extension(state): Extension<WebAccountState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_website_origin(&state.config, &headers)?;
    if let Some(token) = token(&headers, &state.config) {
        state.service.logout(&token).await.map_err(error)?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&state.config, "", 0)?);
    Ok(response)
}
