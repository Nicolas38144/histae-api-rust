use super::{
    domain::{RewardIntent, RewardPlatform},
    error::RewardError,
    ssv::{AdMobVerifier, VerificationError, VerifiedCallback},
    store::RewardStore,
};
use crate::{
    config::LimitPolicy,
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson, ValidatedPath},
        lifecycle::ClientIp,
        rate_limit::RateLimiter,
        router::HttpState,
    },
    identity::mobile::http::{MobileAuthState, OnboardedMobile},
    operations::logging,
};
use axum::{
    Json, Router,
    extract::{Extension, RawQuery},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct AdvertisingHttpState {
    repository: Arc<dyn RewardStore>,
    verifier: AdMobVerifier,
    limiter: RateLimiter,
    user_policy: LimitPolicy,
    webhook_policy: LimitPolicy,
}

impl AdvertisingHttpState {
    pub fn new(
        repository: Arc<dyn RewardStore>,
        verifier: AdMobVerifier,
        limiter: RateLimiter,
        user_policy: LimitPolicy,
        webhook_policy: LimitPolicy,
    ) -> Self {
        Self {
            repository,
            verifier,
            limiter,
            user_policy,
            webhook_policy,
        }
    }
}

pub fn routes(state: AdvertisingHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/advertising-rewards", post(create))
        .route(
            "/api/users/me/advertising-rewards/{id}",
            get(status).delete(cancel),
        )
        .route("/api/auth/admob/webhook", get(webhook))
        .layer(Extension(state))
        .layer(Extension(auth))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    platform: RewardPlatform,
}
impl ApiDto for CreateBody {
    const ERROR_CODE: &'static str = "invalid_reward_request";
    const ERROR_MESSAGE: &'static str = "Platform must be android or ios.";
}
async fn create(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<AdvertisingHttpState>,
    ValidatedJson(body): ValidatedJson<CreateBody>,
) -> Result<Json<RewardIntent>, ApiError> {
    state
        .limiter
        .enforce(
            "advertising_reward",
            &identity.account.user_id.to_string(),
            &state.user_policy,
            "advertising_rate_limit_exceeded",
        )
        .await?;
    state
        .repository
        .create(identity.account.user_id, body.platform)
        .await
        .map(Json)
        .map_err(reward_error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RewardPath {
    id: uuid::Uuid,
}
impl ApiDto for RewardPath {
    const ERROR_CODE: &'static str = "invalid_reward_id";
    const ERROR_MESSAGE: &'static str = "The reward ID must be a UUID.";
    fn is_valid(&self) -> bool {
        self.id.get_version() == Some(uuid::Version::Random)
    }
}
async fn status(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<AdvertisingHttpState>,
    ValidatedPath(path): ValidatedPath<RewardPath>,
) -> Result<Json<RewardIntent>, ApiError> {
    state
        .repository
        .status(identity.account.user_id, path.id)
        .await
        .map(Json)
        .map_err(reward_error)
}
async fn cancel(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<AdvertisingHttpState>,
    ValidatedPath(path): ValidatedPath<RewardPath>,
) -> Result<StatusCode, ApiError> {
    state
        .repository
        .cancel(identity.account.user_id, path.id)
        .await
        .map_err(reward_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn webhook(
    Extension(state): Extension<AdvertisingHttpState>,
    Extension(ip): Extension<ClientIp>,
    RawQuery(query): RawQuery,
) -> Result<StatusCode, ApiError> {
    state
        .limiter
        .enforce(
            "admob_webhook",
            &ip.0.to_string(),
            &state.webhook_policy,
            "admob_webhook_rate_limit_exceeded",
        )
        .await?;
    if !state.repository.enabled() {
        return Err(reward_error(RewardError::Disabled));
    }
    let callback = state
        .verifier
        .verify(query.as_deref().unwrap_or_default())
        .await
        .map_err(|error| match error {
            VerificationError::Invalid => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_admob_signature",
                "The reward proof is invalid.",
            ),
            VerificationError::Unavailable => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admob_verification_unavailable",
                "Reward verification is temporarily unavailable.",
            ),
        })?;
    complete_verified_callback(state.repository.as_ref(), callback).await
}

async fn complete_verified_callback(
    repository: &dyn RewardStore,
    callback: VerifiedCallback,
) -> Result<StatusCode, ApiError> {
    match callback {
        VerifiedCallback::UrlValidation => {
            let _ = logging::info("admob_callback_url_validated", &[]);
        }
        VerifiedCallback::Reward(callback) => {
            repository.complete(callback).await.map_err(reward_error)?;
        }
    }
    Ok(StatusCode::OK)
}
fn reward_error(error: RewardError) -> ApiError {
    match error {
        RewardError::Disabled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "advertising_disabled",
            "Advertising rewards are not enabled.",
        ),
        RewardError::NotEligible => ApiError::new(
            StatusCode::CONFLICT,
            "advertising_not_eligible",
            "An advertising reward is not available at this time.",
        ),
        RewardError::StockFull => ApiError::new(
            StatusCode::CONFLICT,
            "advertising_stock_full",
            "The advertising credit stock is full.",
        ),
        RewardError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "reward_not_found",
            "The reward could not be found.",
        ),
        RewardError::InvalidProof => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_admob_reward",
            "The reward proof does not match an eligible intent.",
        ),
        RewardError::Database(error) => ApiError::from(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    struct RejectingStore {
        completions: std::sync::atomic::AtomicUsize,
    }

    impl RewardStore for RejectingStore {
        fn enabled(&self) -> bool {
            true
        }

        fn create(
            &self,
            _user_id: uuid::Uuid,
            _platform: RewardPlatform,
        ) -> super::super::store::RewardFuture<'_, RewardIntent> {
            Box::pin(async { Err(RewardError::NotEligible) })
        }

        fn status(
            &self,
            _user_id: uuid::Uuid,
            _id: uuid::Uuid,
        ) -> super::super::store::RewardFuture<'_, RewardIntent> {
            Box::pin(async { Err(RewardError::NotFound) })
        }

        fn cancel(
            &self,
            _user_id: uuid::Uuid,
            _id: uuid::Uuid,
        ) -> super::super::store::RewardFuture<'_, ()> {
            Box::pin(async { Err(RewardError::NotFound) })
        }

        fn complete(
            &self,
            _callback: super::super::ssv::Callback,
        ) -> super::super::store::RewardFuture<'_, ()> {
            self.completions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Err(RewardError::InvalidProof) })
        }
    }

    #[tokio::test]
    async fn url_validation_never_calls_the_reward_store() {
        let repository = RejectingStore {
            completions: std::sync::atomic::AtomicUsize::new(0),
        };
        let status = complete_verified_callback(&repository, VerifiedCallback::UrlValidation)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            repository
                .completions
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn reward_callbacks_keep_the_store_validation() {
        let repository = RejectingStore {
            completions: std::sync::atomic::AtomicUsize::new(0),
        };
        let callback = VerifiedCallback::Reward(super::super::ssv::Callback {
            intent_id: uuid::Uuid::new_v4(),
            ad_unit: "1234567890".to_owned(),
            transaction_id: "123456789".to_owned(),
            timestamp_ms: 1_791_907_200_000,
            reward_amount: 1,
            reward_item: "continuation_credit".to_owned(),
        });
        let error = complete_verified_callback(&repository, callback)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.code(), "invalid_admob_reward");
        assert_eq!(
            repository
                .completions
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn platform_validation_preserves_the_http_error_contract() {
        let router = Router::new().route(
            "/reward",
            post(|ValidatedJson(_): ValidatedJson<CreateBody>| async { StatusCode::NO_CONTENT }),
        );
        for body in [r#"{"platform":"android"}"#, r#"{"platform":"ios"}"#] {
            let response = router
                .clone()
                .oneshot(
                    Request::post("/reward")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        for body in [
            r#"{"platform":"web"}"#,
            r#"{"platform":"IOS"}"#,
            "{}",
            r#"{"platform":"ios","watched":true}"#,
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::post("/reward")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
            let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(error["error"]["code"], "invalid_reward_request");
        }
    }
}
