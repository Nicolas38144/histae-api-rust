use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::{Environment, LimitPolicy, TrustProxy};

use super::error::ApiError;
use super::health::{self, Readiness};
use super::lifecycle::{HttpObserver, LifecycleState, SafeHttpObserver, TrustProxyError};
use super::rate_limit::RateLimiter;

pub const JSON_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct HttpState {
    readiness: Readiness,
    lifecycle: LifecycleState,
    cors_origins: Arc<[HeaderValue]>,
}

impl HttpState {
    pub fn new(
        readiness: Readiness,
        environment: Environment,
        trust_proxy: &TrustProxy,
        cors_origins: &[String],
        limiter: RateLimiter,
        global_policy: LimitPolicy,
    ) -> Result<Self, HttpBuildError> {
        Self::with_observer(
            readiness,
            environment,
            trust_proxy,
            cors_origins,
            limiter,
            global_policy,
            Arc::new(SafeHttpObserver),
        )
    }

    pub fn with_observer(
        readiness: Readiness,
        environment: Environment,
        trust_proxy: &TrustProxy,
        cors_origins: &[String],
        limiter: RateLimiter,
        global_policy: LimitPolicy,
        observer: Arc<dyn HttpObserver>,
    ) -> Result<Self, HttpBuildError> {
        let lifecycle =
            LifecycleState::new(environment, trust_proxy, limiter, global_policy, observer)?;
        let cors_origins = cors_origins
            .iter()
            .map(|origin| {
                origin
                    .parse::<HeaderValue>()
                    .map_err(|_| HttpBuildError::InvalidCorsOrigin)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            readiness,
            lifecycle,
            cors_origins: cors_origins.into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpBuildError {
    InvalidTrustProxy,
    InvalidCorsOrigin,
}

impl From<TrustProxyError> for HttpBuildError {
    fn from(_: TrustProxyError) -> Self {
        Self::InvalidTrustProxy
    }
}

pub fn health_routes() -> Router<HttpState> {
    Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(ready))
}

pub fn build_router(routes: Router<HttpState>, state: HttpState) -> Router {
    let lifecycle = state.lifecycle.clone();
    let cors_origins = Arc::clone(&state.cors_origins);
    let mut router = routes
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(
            lifecycle,
            super::lifecycle::middleware,
        ));
    if !cors_origins.is_empty() {
        let cors_state = CorsResponseState {
            origins: Arc::clone(&cors_origins),
        };
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list(cors_origins.iter().cloned()))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                HeaderName::from_static("authorization"),
                HeaderName::from_static("content-type"),
                HeaderName::from_static("idempotency-key"),
                HeaderName::from_static("x-request-id"),
            ])
            .expose_headers([
                HeaderName::from_static("retry-after"),
                HeaderName::from_static("x-request-id"),
            ])
            .max_age(Duration::from_secs(600));
        router = router.layer(cors);
        router = router.layer(middleware::from_fn_with_state(
            cors_state,
            normalize_cors_response,
        ));
    }
    router.with_state(state)
}

async fn ready(
    State(state): State<HttpState>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    health::ready(&state.readiness).await
}

async fn not_found() -> Response {
    ApiError::route_not_found().into_response()
}

#[derive(Clone)]
struct CorsResponseState {
    origins: Arc<[HeaderValue]>,
}

async fn normalize_cors_response(
    State(state): State<CorsResponseState>,
    request: Request,
    next: Next,
) -> Response {
    let has_origin = request.headers().contains_key(header::ORIGIN);
    let is_preflight = request.method() == Method::OPTIONS
        && has_origin
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if is_preflight {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("Retry-After, X-Request-ID"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type, Idempotency-Key, X-Request-ID"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        if let Some(origin) = request.headers().get(header::ORIGIN)
            && state.origins.iter().any(|allowed| allowed == origin)
        {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        }
        return response;
    }

    let mut response = next.run(request).await;
    if has_origin {
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("Retry-After, X-Request-ID"),
        );
    }
    response
}
