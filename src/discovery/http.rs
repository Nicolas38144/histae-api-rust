use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::{Uuid, Variant};

use super::domain::{DiscoveryStatus, SwipeDecision};
use super::service::{DiscoveryError, DiscoveryService, FeedPage, SwipeResponse};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedQuery};
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};
use crate::matches::service::MatchError;
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct DiscoveryHttpState {
    service: DiscoveryService,
    limiter: RateLimiter,
    feed_policy: LimitPolicy,
    swipe_policy: LimitPolicy,
}

impl DiscoveryHttpState {
    pub fn new(
        service: DiscoveryService,
        limiter: RateLimiter,
        feed_policy: LimitPolicy,
        swipe_policy: LimitPolicy,
    ) -> Self {
        Self {
            service,
            limiter,
            feed_policy,
            swipe_policy,
        }
    }
}

pub fn routes(state: DiscoveryHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/discovery-status", get(status))
        .route("/api/feed", get(feed))
        .route("/api/swipes", post(swipe))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn status(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<DiscoveryHttpState>,
) -> Result<Json<DiscoveryStatus>, ApiError> {
    state
        .service
        .status(identity.account.user_id)
        .await
        .map(Json)
        .map_err(discovery_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedQuery {
    limit: Option<String>,
    cursor: Option<String>,
}

impl FeedQuery {
    fn limit(&self) -> Option<u32> {
        let Some(value) = &self.limit else {
            return Some(20);
        };
        let number = value.trim().parse::<f64>().ok()?;
        if !number.is_finite() || number.fract() != 0.0 || !(1.0..=100.0).contains(&number) {
            return None;
        }
        Some(number as u32)
    }
}

impl ApiDto for FeedQuery {
    const ERROR_CODE: &'static str = "invalid_feed_query";
    const ERROR_MESSAGE: &'static str = "The feed query is invalid.";

    fn is_valid(&self) -> bool {
        self.limit().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

async fn feed(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<DiscoveryHttpState>,
    ValidatedQuery(query): ValidatedQuery<FeedQuery>,
) -> Result<Json<FeedPage>, ApiError> {
    state
        .limiter
        .enforce(
            "feed",
            &identity.account.user_id.to_string(),
            &state.feed_policy,
            "feed_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .feed(
            identity.account.user_id,
            query.limit().ok_or_else(invalid_feed_query)?,
            query.cursor.as_deref(),
        )
        .await
        .map(Json)
        .map_err(discovery_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSwipeBody {
    target_user_id: String,
    decision: SwipeDecision,
}

impl CreateSwipeBody {
    fn target_user_id(&self) -> Option<Uuid> {
        canonical_uuid(&self.target_user_id)
    }
}

impl ApiDto for CreateSwipeBody {
    const ERROR_CODE: &'static str = "invalid_swipe_payload";
    const ERROR_MESSAGE: &'static str = "The swipe request body is invalid.";

    fn is_valid(&self) -> bool {
        self.target_user_id().is_some()
    }
}

async fn swipe(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<DiscoveryHttpState>,
    ValidatedJson(body): ValidatedJson<CreateSwipeBody>,
) -> Result<(StatusCode, Json<SwipeResponse>), ApiError> {
    state
        .limiter
        .enforce(
            "swipes",
            &identity.account.user_id.to_string(),
            &state.swipe_policy,
            "swipe_rate_limit_exceeded",
        )
        .await?;
    let target_id = body.target_user_id().ok_or_else(invalid_swipe_payload)?;
    state
        .service
        .swipe(identity.account.user_id, target_id, body.decision)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
        .map_err(discovery_error)
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

fn invalid_feed_query() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_feed_query",
        "The feed query is invalid.",
    )
}

fn invalid_swipe_payload() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_swipe_payload",
        "The swipe request body is invalid.",
    )
}

fn discovery_error(error: DiscoveryError) -> ApiError {
    match error {
        DiscoveryError::InvalidFeedRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_feed_request",
            "The feed request is invalid.",
        ),
        DiscoveryError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        DiscoveryError::InvalidSwipeRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_swipe_request",
            "The swipe request is invalid.",
        ),
        DiscoveryError::NotReady => ApiError::new(
            StatusCode::CONFLICT,
            "discovery_not_ready",
            "A complete profile, preferences, current consents and a fresh location are required for discovery.",
        ),
        DiscoveryError::CandidateNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "discovery_candidate_not_found",
            "The discovery candidate is not available.",
        ),
        DiscoveryError::SwipeAlreadyRecorded => ApiError::new(
            StatusCode::CONFLICT,
            "swipe_already_recorded",
            "A different swipe decision was already recorded for this user.",
        ),
        DiscoveryError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "discovery_unavailable",
            "Discovery is temporarily unavailable.",
        ),
        DiscoveryError::Database(error) => error.into(),
        DiscoveryError::AccountActivity(error) => error.into(),
        DiscoveryError::Match(error) => match_error(error),
    }
}

fn match_error(error: MatchError) -> ApiError {
    match error {
        MatchError::InvalidRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_match_request",
            "The match request is invalid.",
        ),
        MatchError::Blocked => ApiError::new(
            StatusCode::CONFLICT,
            "match_blocked",
            "A match cannot be created between blocked users.",
        ),
        MatchError::CandidateNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "discovery_candidate_not_found",
            "The discovery candidate is no longer available.",
        ),
        MatchError::Database(error) => error.into(),
        _ => ApiError::internal(),
    }
}
