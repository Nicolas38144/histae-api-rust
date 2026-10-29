mod dto;
pub(crate) mod security;
use super::{ChallengeView, WebBillingError, WebBillingService, WebSessionView};
use crate::{
    billing::domain::CheckoutSessionView,
    catalog::{domain::PlanCatalog, service::CatalogService},
    config::{LimitPolicy, WebBillingConfig},
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
use dto::{CheckoutBody, SendBody, VerifyBody};
use security::{set_cookie, token, validate_origin, validate_website_origin};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub struct WebBillingState {
    pub service: WebBillingService,
    pub catalog: CatalogService,
    pub config: WebBillingConfig,
    pub limiter: RateLimiter,
    pub otp_policy: LimitPolicy,
    pub billing_policy: LimitPolicy,
}
pub fn routes(state: WebBillingState) -> Router<HttpState> {
    Router::new()
        .route("/api/web/billing/otp/send", post(send))
        .route("/api/web/billing/otp/verify", post(verify))
        .route("/api/web/billing/session", get(session).delete(logout))
        .route("/api/web/account/otp/send", post(send))
        .route("/api/web/account/otp/verify", post(verify))
        .route(
            "/api/web/account/session",
            get(account_session).delete(logout),
        )
        .route("/api/web/billing/plans", get(plans))
        .route("/api/web/billing/checkout", post(checkout))
        .route("/api/web/billing/portal", post(portal))
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
pub(crate) fn error(error: WebBillingError) -> ApiError {
    let (status, code, message) = match error {
        WebBillingError::InvalidRequest => return ApiError::invalid_body(),
        WebBillingError::Authentication => (
            StatusCode::UNAUTHORIZED,
            "web_authentication_failed",
            "The account or verification code is invalid or expired.",
        ),
        WebBillingError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "idempotency_key_conflict",
            "Use the original parameters for this request.",
        ),
        WebBillingError::OnboardingIncomplete => (
            StatusCode::FORBIDDEN,
            "onboarding_incomplete",
            "Complete onboarding in the application before purchasing.",
        ),
        WebBillingError::Internal => return ApiError::internal(),
        WebBillingError::Database(error) => return error.into(),
        WebBillingError::Billing(error) => return crate::billing::http::billing_error(error),
    };
    ApiError::new(status, code, message)
}
fn session_token(state: &WebBillingState, headers: &HeaderMap) -> Result<String, ApiError> {
    token(headers, &state.config).ok_or_else(|| error(WebBillingError::Authentication))
}
async fn limit_otp(
    state: &WebBillingState,
    phone: &str,
    ip: std::net::IpAddr,
    verify: bool,
) -> Result<(), ApiError> {
    let hash = state.service.phone_key(phone).map_err(|e| {
        if verify {
            error(WebBillingError::Authentication)
        } else {
            error(e)
        }
    })?;
    let namespaces = if verify {
        ["otp-verify-ip", "otp-verify-phone"]
    } else {
        ["otp-send-ip", "otp-send-phone"]
    };
    state
        .limiter
        .enforce(
            namespaces[0],
            &ip.to_string(),
            &state.otp_policy,
            "otp_rate_limit_exceeded",
        )
        .await?;
    state
        .limiter
        .enforce(
            namespaces[1],
            &hash,
            &state.otp_policy,
            "otp_rate_limit_exceeded",
        )
        .await
}
async fn send(
    Extension(state): Extension<WebBillingState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<SendBody>,
) -> Result<(StatusCode, Json<ChallengeView>), ApiError> {
    validate_website_origin(&state.config, &headers)?;
    limit_otp(&state, &body.phone_number, ip, false).await?;
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
    Extension(state): Extension<WebBillingState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<VerifyBody>,
) -> Result<Response, ApiError> {
    validate_website_origin(&state.config, &headers)?;
    limit_otp(&state, &body.phone_number, ip, true).await?;
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
async fn session(
    Extension(state): Extension<WebBillingState>,
    headers: HeaderMap,
) -> Result<Json<WebSessionView>, ApiError> {
    state
        .service
        .session(&session_token(&state, &headers)?)
        .await
        .map(Json)
        .map_err(error)
}
async fn account_session(
    Extension(state): Extension<WebBillingState>,
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
    Extension(state): Extension<WebBillingState>,
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
async fn plans(
    Extension(state): Extension<WebBillingState>,
    locale: Locale,
) -> Result<Json<PlanCatalog>, ApiError> {
    crate::catalog::http::localized_plans(&state.catalog, locale)
        .await
        .map(Json)
}
async fn payer(state: &WebBillingState, headers: &HeaderMap) -> Result<Uuid, ApiError> {
    validate_origin(&state.config, headers)?;
    let user = state
        .service
        .payer(&session_token(state, headers)?)
        .await
        .map_err(error)?;
    state
        .limiter
        .enforce(
            "billing",
            &user.to_string(),
            &state.billing_policy,
            "billing_rate_limit_exceeded",
        )
        .await?;
    Ok(user)
}
async fn checkout(
    Extension(state): Extension<WebBillingState>,
    headers: HeaderMap,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<CheckoutBody>,
) -> Result<(StatusCode, Json<CheckoutSessionView>), ApiError> {
    let user = payer(&state, &headers).await?;
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    state
        .service
        .checkout(user, body.billing_period, key, locale.anonymous().language)
        .await
        .map(|view| (StatusCode::CREATED, Json(view)))
        .map_err(error)
}
async fn portal(
    Extension(state): Extension<WebBillingState>,
    headers: HeaderMap,
    locale: Locale,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = payer(&state, &headers).await?;
    state
        .service
        .portal(user, locale.anonymous().language)
        .await
        .map(|url| (StatusCode::CREATED, Json(json!({"url": url}))))
        .map_err(error)
}
