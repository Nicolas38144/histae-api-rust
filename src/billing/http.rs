use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::domain::{BillingPeriod, CheckoutSessionView, SubscriptionView};
use super::service::{BillingError, BillingService};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson};
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};

#[derive(Clone)]
pub struct BillingHttpState {
    service: Arc<dyn BillingUseCases>,
    limiter: RateLimiter,
    policy: LimitPolicy,
}

impl BillingHttpState {
    pub fn new(
        service: Arc<dyn BillingUseCases>,
        limiter: RateLimiter,
        policy: LimitPolicy,
    ) -> Self {
        Self {
            service,
            limiter,
            policy,
        }
    }
}

pub type BillingHttpFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BillingError>> + Send + 'a>>;

pub trait BillingUseCases: Send + Sync {
    fn subscription(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, SubscriptionView>;

    fn create_checkout(
        &self,
        user_id: uuid::Uuid,
        billing_period: BillingPeriod,
        idempotency_key: Option<&str>,
    ) -> BillingHttpFuture<'_, CheckoutSessionView>;

    fn create_portal(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, String>;
}

impl BillingUseCases for BillingService {
    fn subscription(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, SubscriptionView> {
        Box::pin(BillingService::subscription(self, user_id))
    }

    fn create_checkout(
        &self,
        user_id: uuid::Uuid,
        billing_period: BillingPeriod,
        idempotency_key: Option<&str>,
    ) -> BillingHttpFuture<'_, CheckoutSessionView> {
        let idempotency_key = idempotency_key.map(str::to_owned);
        Box::pin(async move {
            BillingService::create_checkout(
                self,
                user_id,
                billing_period,
                idempotency_key.as_deref(),
            )
            .await
        })
    }

    fn create_portal(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, String> {
        Box::pin(BillingService::create_portal(self, user_id))
    }
}

pub fn routes(state: BillingHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/subscription", get(subscription))
        .route("/api/users/me/subscription/checkout", post(create_checkout))
        .route("/api/users/me/subscription/portal", post(create_portal))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn subscription(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<BillingHttpState>,
) -> Result<Json<SubscriptionView>, ApiError> {
    state
        .service
        .subscription(identity.account.user_id)
        .await
        .map(Json)
        .map_err(billing_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCheckoutBody {
    billing_period: BillingPeriod,
}

impl ApiDto for CreateCheckoutBody {
    const ERROR_CODE: &'static str = "invalid_checkout_payload";
    const ERROR_MESSAGE: &'static str = "The Checkout request body is invalid.";

    fn is_valid(&self) -> bool {
        true
    }
}

async fn create_checkout(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<BillingHttpState>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<CreateCheckoutBody>,
) -> Result<(StatusCode, Json<CheckoutSessionView>), ApiError> {
    let user_id = identity.account.user_id;
    state
        .limiter
        .enforce(
            "billing",
            &user_id.hyphenated().to_string(),
            &state.policy,
            "billing_rate_limit_exceeded",
        )
        .await?;
    let idempotency_key = single_header(&headers, "idempotency-key");
    state
        .service
        .create_checkout(user_id, body.billing_period, idempotency_key)
        .await
        .map(|session| (StatusCode::CREATED, Json(session)))
        .map_err(billing_error)
}

#[derive(Serialize)]
struct PortalResponse {
    url: String,
}

async fn create_portal(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<BillingHttpState>,
) -> Result<(StatusCode, Json<PortalResponse>), ApiError> {
    let user_id = identity.account.user_id;
    state
        .limiter
        .enforce(
            "billing",
            &user_id.hyphenated().to_string(),
            &state.policy,
            "billing_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .create_portal(user_id)
        .await
        .map(|url| (StatusCode::CREATED, Json(PortalResponse { url })))
        .map_err(billing_error)
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn billing_error(error: BillingError) -> ApiError {
    match error {
        BillingError::InvalidIdempotencyKey => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "The Idempotency-Key header must contain a UUID v4.",
        ),
        BillingError::BillingUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Stripe billing is temporarily unavailable.",
        ),
        BillingError::AccountNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "account_not_found",
            "The account could not be found or has been deleted.",
        ),
        BillingError::SubscriptionAlreadyActive => ApiError::new(
            StatusCode::CONFLICT,
            "subscription_already_active",
            "A Premium subscription is already active.",
        ),
        BillingError::CheckoutAlreadyInProgress => ApiError::new(
            StatusCode::CONFLICT,
            "checkout_already_in_progress",
            "A Checkout session is already being created or is still open.",
        ),
        BillingError::CustomerReconciliationRequired => ApiError::new(
            StatusCode::CONFLICT,
            "billing_customer_reconciliation_required",
            "An earlier Stripe customer creation must be resolved before starting another Checkout.",
        ),
        BillingError::IdempotencyKeyReused => ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_key_reused",
            "This Idempotency-Key was already used with a different billing period.",
        ),
        BillingError::IdempotencyKeyConsumed => ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_key_consumed",
            "This Idempotency-Key belongs to a completed or expired Checkout session.",
        ),
        BillingError::BillingCustomerConflict => ApiError::new(
            StatusCode::CONFLICT,
            "billing_customer_conflict",
            "The Stripe customer could not be attached to this account.",
        ),
        BillingError::CheckoutStateConflict => ApiError::new(
            StatusCode::CONFLICT,
            "checkout_state_conflict",
            "The Checkout session could not be persisted safely.",
        ),
        BillingError::StripeCheckoutUnavailable => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "stripe_checkout_unavailable",
            "Stripe did not return a hosted Checkout URL.",
        ),
        BillingError::StripeRequestFailed => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "stripe_request_failed",
            "Stripe could not process the billing request at this time.",
        ),
        BillingError::BillingCustomerNotFound => ApiError::new(
            StatusCode::CONFLICT,
            "billing_customer_not_found",
            "No Stripe customer exists for this account yet.",
        ),
        BillingError::ErasureStripeReconciliationRequired => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "erasure_stripe_reconciliation_required",
            "The Stripe customer creation requires reconciliation.",
        ),
        BillingError::DataErasureUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "data_erasure_unavailable",
            "The Stripe customer could not be erased at this time.",
        ),
        BillingError::Database(error) => error.into(),
        BillingError::AccountActivity(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    #[test]
    fn rejects_duplicate_idempotency_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_static("first"),
        );
        headers.append(
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_static("second"),
        );
        assert_eq!(single_header(&headers, "idempotency-key"), None);
    }
}
