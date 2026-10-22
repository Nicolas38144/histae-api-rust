use super::{PurchaseIntent, StoreBillingService, StoreError, StoreProvider};
use crate::{
    config::LimitPolicy,
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson, ValidatedQuery},
        lifecycle::ClientIp,
        rate_limit::RateLimiter,
        router::HttpState,
    },
    identity::mobile::http::{MobileAuthState, OnboardedMobile},
};
use axum::{
    Json, Router,
    extract::Extension,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct StoreHttpState {
    pub service: StoreBillingService,
    pub limiter: RateLimiter,
    pub policy: LimitPolicy,
    pub webhook_policy: LimitPolicy,
}
pub fn routes(state: StoreHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/subscription/purchase-intents", post(intent))
        .route("/api/users/me/subscription/apple/verify", post(apple))
        .route(
            "/api/users/me/subscription/google-play/verify",
            post(google),
        )
        .layer(Extension(state))
        .layer(Extension(auth))
}
pub fn webhook_routes(state: StoreHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/billing/store-products", get(products))
        .route("/api/billing/apple/webhook", post(apple_notification))
        .route(
            "/api/billing/google-play/webhook",
            post(google_notification),
        )
        .layer(Extension(state))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentBody {
    provider: StoreProvider,
}
impl ApiDto for IntentBody {}
async fn products(
    Extension(state): Extension<StoreHttpState>,
    ValidatedQuery(query): ValidatedQuery<IntentBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"provider":query.provider,"offers":state.service.products(query.provider).map_err(error)?}),
    ))
}
async fn intent(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<StoreHttpState>,
    ValidatedJson(body): ValidatedJson<IntentBody>,
) -> Result<Json<PurchaseIntent>, ApiError> {
    state
        .limiter
        .enforce(
            "billing",
            &identity.account.user_id.to_string(),
            &state.policy,
            "billing_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .intent(identity.account.user_id, body.provider)
        .await
        .map(Json)
        .map_err(error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyBody {
    reference: String,
}
impl ApiDto for VerifyBody {
    fn is_valid(&self) -> bool {
        !self.reference.is_empty() && self.reference.len() <= 4096
    }
}
async fn apple(
    identity: OnboardedMobile,
    state: Extension<StoreHttpState>,
    body: ValidatedJson<VerifyBody>,
) -> Result<StatusCode, ApiError> {
    verify(identity, state, body, StoreProvider::AppStore).await
}
async fn google(
    identity: OnboardedMobile,
    state: Extension<StoreHttpState>,
    body: ValidatedJson<VerifyBody>,
) -> Result<StatusCode, ApiError> {
    verify(identity, state, body, StoreProvider::GooglePlay).await
}
async fn verify(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<StoreHttpState>,
    ValidatedJson(body): ValidatedJson<VerifyBody>,
    provider: StoreProvider,
) -> Result<StatusCode, ApiError> {
    state
        .limiter
        .enforce(
            "billing",
            &identity.account.user_id.to_string(),
            &state.policy,
            "billing_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .verify(Some(identity.account.user_id), provider, body.reference)
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(transparent)]
struct NotificationBody(Value);
impl ApiDto for NotificationBody {
    fn is_valid(&self) -> bool {
        self.0.is_object()
    }
}
async fn apple_notification(
    state: Extension<StoreHttpState>,
    ip: Extension<ClientIp>,
    headers: HeaderMap,
    ValidatedJson(NotificationBody(body)): ValidatedJson<NotificationBody>,
) -> Result<Json<Value>, ApiError> {
    notification(state, ip, headers, body, StoreProvider::AppStore).await
}
async fn google_notification(
    state: Extension<StoreHttpState>,
    ip: Extension<ClientIp>,
    headers: HeaderMap,
    ValidatedJson(NotificationBody(body)): ValidatedJson<NotificationBody>,
) -> Result<Json<Value>, ApiError> {
    notification(state, ip, headers, body, StoreProvider::GooglePlay).await
}
async fn notification(
    Extension(state): Extension<StoreHttpState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    body: Value,
    provider: StoreProvider,
) -> Result<Json<Value>, ApiError> {
    state
        .limiter
        .enforce(
            "store-webhook",
            &ip.to_string(),
            &state.webhook_policy,
            "billing_webhook_rate_limit_exceeded",
        )
        .await?;
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    state
        .service
        .notification(provider, body, authorization)
        .await
        .map_err(error)?;
    Ok(Json(json!({"received":true})))
}
pub fn error(value: StoreError) -> ApiError {
    let (status, code) = match value {
        StoreError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "store_unavailable"),
        StoreError::Invalid => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_store_purchase"),
        StoreError::Ownership => (StatusCode::CONFLICT, "store_purchase_account_conflict"),
        StoreError::AlreadySubscribed => (StatusCode::CONFLICT, "subscription_already_active"),
        StoreError::AccountUnavailable => (StatusCode::UNAUTHORIZED, "authentication_required"),
        StoreError::Database(error) => return error.into(),
    };
    ApiError::new(
        status,
        code,
        "The purchase could not be processed. Check the account and try again.",
    )
}
