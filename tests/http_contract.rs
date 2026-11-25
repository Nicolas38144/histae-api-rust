use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::Path;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use histae_api_rust::config::{Environment, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness, WebauthnError};
use histae_api_rust::http::lifecycle::{HttpObservation, HttpObserver};
use histae_api_rust::http::rate_limit::{FixedWindowStore, RateLimiter, StoreError, StoreFuture};
use histae_api_rust::http::router::{HttpState, build_router, health_routes};
use histae_api_rust::infra::redis::FixedWindowIncrement;
use histae_api_rust::operations::{metrics::OperationalMetrics, prometheus::render_snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
struct Probe {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
    fails: bool,
}

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move {
            self.calls.lock().expect("probe lock").push(self.name);
            if self.fails {
                Err(WebauthnError)
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Default)]
struct Observations(Mutex<Vec<HttpObservation>>);

impl HttpObserver for Observations {
    fn record(&self, observation: HttpObservation) {
        self.0.lock().expect("observation lock").push(observation);
    }
}

struct FailingStore;

impl FixedWindowStore for FailingStore {
    fn increment<'a>(&'a self, _key: &'a str, _window: Duration) -> StoreFuture<'a> {
        Box::pin(async { Err(StoreError) })
    }
}

struct FixedStore(FixedWindowIncrement);

impl FixedWindowStore for FixedStore {
    fn increment<'a>(&'a self, _key: &'a str, _window: Duration) -> StoreFuture<'a> {
        let value = self.0;
        Box::pin(async move { Ok(value) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SampleBody {
    name: String,
}

impl ApiDto for SampleBody {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomBody {
    value: u8,
}

impl ApiDto for CustomBody {
    const ERROR_CODE: &'static str = "invalid_custom_body";
    const ERROR_MESSAGE: &'static str = "The custom body is invalid.";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SampleQuery {
    #[serde(default = "default_limit")]
    limit: u8,
}

impl ApiDto for SampleQuery {
    const ERROR_CODE: &'static str = "invalid_sample_query";
    const ERROR_MESSAGE: &'static str = "The sample query is invalid.";
}

fn default_limit() -> u8 {
    20
}

#[derive(Deserialize)]
struct SamplePath {
    id: Uuid,
}

impl ApiDto for SamplePath {
    const ERROR_CODE: &'static str = "invalid_sample_id";
    const ERROR_MESSAGE: &'static str = "The sample identifier is invalid.";
}

#[derive(Serialize)]
struct SampleResponse {
    name: String,
}

async fn sample_body(ValidatedJson(body): ValidatedJson<SampleBody>) -> Json<SampleResponse> {
    Json(SampleResponse { name: body.name })
}

async fn custom_body(ValidatedJson(body): ValidatedJson<CustomBody>) -> Json<Value> {
    Json(json!({ "value": body.value }))
}

async fn sample_query(ValidatedQuery(query): ValidatedQuery<SampleQuery>) -> Json<Value> {
    Json(json!({ "limit": query.limit }))
}

async fn sample_path(ValidatedPath(path): ValidatedPath<SamplePath>) -> Json<Value> {
    Json(json!({ "id": path.id }))
}

fn policy(max: u64) -> LimitPolicy {
    LimitPolicy {
        max,
        window: Duration::from_secs(60),
    }
}

fn hash_key() -> SecretString {
    SecretString::new("hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh".to_owned())
}

fn readiness(failure: Option<&'static str>) -> (Readiness, Arc<Mutex<Vec<&'static str>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let probe = |name| {
        Arc::new(Probe {
            name,
            calls: Arc::clone(&calls),
            fails: failure == Some(name),
        }) as Arc<dyn DependencyProbe>
    };
    (
        Readiness::new(probe("postgres"), probe("redis"), probe("storage")),
        calls,
    )
}

fn app(
    failure: Option<&'static str>,
    environment: Environment,
    cors: &[&str],
    limiter: RateLimiter,
    max: u64,
    observer: Arc<dyn HttpObserver>,
) -> (Router, Arc<Mutex<Vec<&'static str>>>) {
    let (readiness, calls) = readiness(failure);
    let origins = cors
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let state = HttpState::with_observer(
        readiness,
        environment,
        &TrustProxy::Disabled,
        &origins,
        limiter,
        policy(max),
        observer,
    )
    .expect("valid HTTP test state");
    let routes = health_routes()
        .route("/api/sample", post(sample_body).get(sample_query))
        .route("/api/custom", post(custom_body))
        .route("/api/sample/{id}", get(sample_path));
    (build_router(routes, state), calls)
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn live_health_preserves_body_head_routing_and_defensive_headers() {
    let observer = Arc::new(Observations::default());
    let (app, _) = app(
        None,
        Environment::Test,
        &[],
        RateLimiter::memory(&hash_key()),
        100,
        observer.clone(),
    );
    let request_id = Uuid::new_v4().to_string().to_ascii_uppercase();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .header("x-request-id", &request_id)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-request-id"],
        request_id.to_ascii_lowercase()
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(response_json(response).await, json!({ "status": "ok" }));

    let invalid_id = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .header("x-request-id", "private-invalid-id")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let generated = invalid_id.headers()["x-request-id"]
        .to_str()
        .expect("request id");
    assert_eq!(
        Uuid::parse_str(generated)
            .expect("generated UUID")
            .get_version_num(),
        4
    );

    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        to_bytes(head.into_body(), usize::MAX)
            .await
            .expect("body")
            .is_empty()
    );

    for request in [
        Request::builder()
            .method(Method::POST)
            .uri("/health/live")
            .body(Body::empty())
            .expect("request"),
        Request::builder()
            .uri("/health/live/")
            .body(Body::empty())
            .expect("request"),
    ] {
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response_json(response).await,
            json!({ "error": { "code": "route_not_found", "message": "This route is not available." } })
        );
    }

    let observations = observer.0.lock().expect("observations");
    assert_eq!(observations[0].route(), "/health/live");
    assert_eq!(observations[2].method(), "HEAD");
    assert_eq!(observations[2].route(), "/health/live");
    assert_eq!(observations[3].route(), "<unmatched>");
    assert_eq!(observations[4].route(), "<unmatched>");
}

#[tokio::test]
async fn http_metrics_group_routing_rejections_and_keep_registered_routes_visible() {
    const ROUTES: usize = 205;
    let metrics = Arc::new(OperationalMetrics::new());
    let state = HttpState::with_observer(
        readiness(None).0,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        RateLimiter::memory(&hash_key()),
        policy(500),
        metrics.clone(),
    )
    .expect("HTTP state");
    let mut routes =
        Router::new().route(
            "/fixture/status/{code}",
            get(|Path(code): Path<u16>| async move {
                StatusCode::from_u16(code).expect("fixture code")
            }),
        );
    for index in 0..ROUTES {
        routes = routes.route(
            &format!("/fixture/route-{index}"),
            get(|| async { StatusCode::NO_CONTENT }),
        );
    }
    let app = build_router(routes, state);
    let send = |method: &str, path: &str| {
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
    };

    for (method, path) in [
        ("CUSTOM", "/fixture/status/401"),
        ("ANNOTATE", "/missing"),
        ("POST", "/fixture/status/401"),
        ("GET", "/missing/one"),
        ("GET", "/missing/two"),
    ] {
        assert_eq!(
            send(method, path).await.expect("response").status(),
            StatusCode::NOT_FOUND
        );
    }
    for index in 0..ROUTES {
        assert_eq!(
            send("GET", &format!("/fixture/route-{index}"))
                .await
                .expect("response")
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    for code in [401, 403, 404, 429, 500] {
        assert_eq!(
            send("GET", &format!("/fixture/status/{code}"))
                .await
                .expect("response")
                .status()
                .as_u16(),
            code
        );
    }

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.http.len(), ROUTES + 1 + 3);
    let scrape = render_snapshot(&snapshot, None, true);
    for (method, count) in [("GET", 2), ("POST", 1), ("OTHER", 2)] {
        let expected = format!(
            "histae_http_requests_total{{method=\"{method}\",route=\"<unmatched>\"}} {count}"
        );
        assert!(scrape.lines().any(|line| line == expected), "{expected}");
    }
    let route_labels = "method=\"GET\",route=\"/fixture/status/{code}\"";
    for status in ["401", "403", "429", "5xx"] {
        let expected =
            format!("histae_http_responses_total{{{route_labels},status=\"{status}\"}} 1");
        assert!(scrape.lines().any(|line| line == expected), "{expected}");
    }
}

#[tokio::test]
async fn production_adds_hsts_and_rate_limit_failures_keep_the_common_headers() {
    let observer = Arc::new(Observations::default());
    let limiter = RateLimiter::with_store(
        &hash_key(),
        Arc::new(FixedStore(FixedWindowIncrement {
            count: 2,
            ttl_millis: 9_500,
        })),
    );
    let (app, _) = app(
        None,
        Environment::Production,
        &[],
        limiter,
        1,
        observer.clone(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "10");
    assert_eq!(
        response.headers()[header::STRICT_TRANSPORT_SECURITY],
        "max-age=31536000; includeSubDomains"
    );
    assert!(response.headers().contains_key("x-request-id"));
    assert_eq!(
        response_json(response).await,
        json!({ "error": { "code": "rate_limit_exceeded", "message": "Too many requests were sent. Please try again later." } })
    );
    let rejected = app
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    let observations = observer.0.lock().expect("observations");
    assert_eq!(observations[0].route(), "/health/live");
    assert_eq!(observations[1].route(), "<unmatched>");
}

#[tokio::test]
async fn redis_failure_fails_closed_and_webhook_paths_skip_only_the_global_limit() {
    let observer = Arc::new(Observations::default());
    let (failed, _) = app(
        None,
        Environment::Test,
        &[],
        RateLimiter::with_store(&hash_key(), Arc::new(FailingStore)),
        100,
        observer.clone(),
    );
    let response = failed
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await,
        json!({ "error": { "code": "rate_limit_unavailable", "message": "Request protection is temporarily unavailable." } })
    );

    let (readiness, _) = readiness(None);
    let state = HttpState::with_observer(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        RateLimiter::with_store(&hash_key(), Arc::new(FailingStore)),
        policy(100),
        observer,
    )
    .expect("valid state");
    let app = build_router(
        Router::new().route(
            "/api/auth/sweego/webhook",
            post(|| async { StatusCode::NO_CONTENT }),
        ),
        state,
    );
    for (method, expected) in [
        (Method::POST, StatusCode::NO_CONTENT),
        (Method::PUT, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/auth/sweego/webhook")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn store_webhooks_have_separate_body_slots_and_ip_quotas() {
    let (readiness, _) = readiness(None);
    let state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        RateLimiter::memory(&hash_key()),
        policy(1),
    )
    .expect("valid state")
    .with_store_webhook_policy(policy(5));
    let apple = "/api/billing/apple/webhook";
    let google = "/api/billing/google-play/webhook";
    let handler = || async { StatusCode::NO_CONTENT };
    let app = build_router(
        health_routes()
            .route(apple, post(handler))
            .route(google, post(handler)),
        state,
    );
    let request = |path, body| {
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .expect("webhook request")
    };

    let (started, mut reads) = tokio::sync::mpsc::channel(4);
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut pending = Vec::new();
    for _ in 0..4 {
        let started = started.clone();
        let release = release.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            started.send(()).await.expect("test reading body starts");
            let _permit = release.acquire().await.expect("release body read");
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"{}"))
        }));
        pending.push(tokio::spawn(app.clone().oneshot(request(apple, body))));
    }
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(2), reads.recv())
            .await
            .expect("Apple body reading started");
    }

    let blocked = app
        .clone()
        .oneshot(request(apple, Body::from("{}")))
        .await
        .expect("response");
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(blocked).await["error"]["code"],
        "request_body_capacity_reached"
    );
    let allowed = app
        .clone()
        .oneshot(request(google, Body::from("{}")))
        .await
        .expect("response");
    assert_eq!(allowed.status(), StatusCode::NO_CONTENT);

    release.add_permits(4);
    for response in pending {
        assert_eq!(
            response.await.expect("joined").expect("response").status(),
            StatusCode::NO_CONTENT
        );
    }
    let rate_limited = app
        .clone()
        .oneshot(request(apple, Body::from("{")))
        .await
        .expect("response");
    assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response_json(rate_limited).await["error"]["code"],
        "billing_webhook_rate_limit_exceeded"
    );
    let general = app
        .oneshot(
            Request::get("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(general.status(), StatusCode::OK);
}

#[tokio::test]
async fn readiness_shares_a_single_recent_result_and_reports_dependency_failure() {
    let observer = Arc::new(Observations::default());
    let (healthy, calls) = app(
        None,
        Environment::Test,
        &[],
        RateLimiter::memory(&hash_key()),
        100,
        observer.clone(),
    );
    let response = healthy
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await, json!({ "status": "ready" }));
    let response = healthy
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("cached response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *calls.lock().expect("calls"),
        ["postgres", "redis", "storage"]
    );

    let (failed, calls) = app(
        Some("redis"),
        Environment::Test,
        &[],
        RateLimiter::memory(&hash_key()),
        100,
        observer,
    );
    let response = failed
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await,
        json!({ "error": { "code": "request_failed", "message": "A required dependency is unavailable." } })
    );
    assert_eq!(*calls.lock().expect("calls"), ["postgres", "redis"]);
}

#[tokio::test]
async fn cors_preflight_matches_fastify_and_bypasses_the_http_lifecycle() {
    let observer = Arc::new(Observations::default());
    let (allowed_app, _) = app(
        None,
        Environment::Test,
        &["https://mobile.example"],
        RateLimiter::memory(&hash_key()),
        100,
        observer.clone(),
    );
    let response = allowed_app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/sample")
                .header(header::ORIGIN, "https://mobile.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://mobile.example"
    );
    assert_eq!(response.headers()[header::ACCESS_CONTROL_MAX_AGE], "600");
    assert_eq!(response.headers()[header::VARY], "Origin");
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS],
        "Retry-After, X-Request-ID"
    );
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET, POST, PUT, PATCH, DELETE, OPTIONS"
    );
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS],
        "Authorization, Content-Type, Idempotency-Key, X-Request-ID, Accept-Language"
    );
    assert!(!response.headers().contains_key("x-request-id"));
    assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    assert!(observer.0.lock().expect("observations").is_empty());

    let (app, _) = app(
        None,
        Environment::Test,
        &["https://mobile.example"],
        RateLimiter::memory(&hash_key()),
        100,
        observer,
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/sample")
                .header(header::ORIGIN, "https://rejected.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
    assert_eq!(response.headers()[header::VARY], "Origin");
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS],
        "Retry-After, X-Request-ID"
    );
    assert!(!response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn explicit_extractors_keep_dto_errors_defaults_and_the_one_megabyte_limit() {
    let observer = Arc::new(Observations::default());
    let (app, _) = app(
        None,
        Environment::Test,
        &[],
        RateLimiter::memory(&hash_key()),
        100,
        observer,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/sample")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"Ada"}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await, json!({ "name": "Ada" }));

    for body in [r#"{"name":"Ada","admin":true}"#, r#"{"missing":true}"#, "{"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sample")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "invalid_request_body"
        );
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/sample")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request_body"
    );

    let oversized = format!(r#"{{"name":"{}"}}"#, "a".repeat(1024 * 1024));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/sample")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(oversized))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request_body"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sample")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response_json(response).await, json!({ "limit": 20 }));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sample?limit=invalid")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_sample_query"
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sample/not-a-uuid")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_sample_id"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/custom")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"value":1,"unknown":true}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_custom_body"
    );

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/custom")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request_body"
    );
}

#[tokio::test]
async fn bodies_are_validated_before_handlers_without_body_extractors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mutations = Arc::new(AtomicUsize::new(0));
    let count = mutations.clone();
    let (readiness, _) = readiness(None);
    let state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        RateLimiter::memory(&hash_key()),
        policy(100),
    )
    .expect("state");
    let routes = Router::new().route(
        "/api/bodyless",
        post(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Json(json!({"ok":true}))
            }
        }),
    );
    let app = build_router(routes, state);
    for (content_type, body, expected) in [
        ("application/json", "{".to_owned(), StatusCode::BAD_REQUEST),
        ("application/json", String::new(), StatusCode::BAD_REQUEST),
        (
            "application/octet-stream",
            String::new(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/octet-stream",
            "unexpected".to_owned(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/octet-stream",
            "x".repeat(1024 * 1024 + 1),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/json",
            format!("\"{}\"", "x".repeat(1024 * 1024)),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/bodyless")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), expected);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "invalid_request_body"
        );
    }
    assert_eq!(mutations.load(Ordering::Relaxed), 0);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/bodyless")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(mutations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn tcp_peer_cannot_spoof_a_trusted_loopback_proxy_and_has_its_own_quota() {
    use histae_api_rust::http::lifecycle::ClientIp;
    let (readiness, _) = readiness(None);
    let state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Networks(vec!["127.0.0.1/32".into()]),
        &[],
        RateLimiter::memory(&hash_key()),
        policy(1),
    )
    .expect("state");
    let routes=Router::new().route("/ip",get(|axum::extract::Extension(ClientIp(ip)): axum::extract::Extension<ClientIp>| async move {Json(json!({"ip":ip.to_string()}))}));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            histae_api_rust::http::router::connected_service(build_router(routes, state)),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await
        .expect("serve");
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .local_address("127.0.0.2".parse::<std::net::IpAddr>().expect("ip"))
        .build()
        .expect("client");
    let url = format!("http://{address}/ip");
    let response = client
        .get(&url)
        .header("x-forwarded-for", "198.51.100.7")
        .send()
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.expect("json")["ip"],
        "127.0.0.2"
    );
    assert_eq!(
        client
            .get(&url)
            .header("x-forwarded-for", "198.51.100.8")
            .send()
            .await
            .expect("response")
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let other = reqwest::Client::builder()
        .no_proxy()
        .local_address("127.0.0.3".parse::<std::net::IpAddr>().expect("ip"))
        .build()
        .expect("client");
    assert_eq!(
        other.get(&url).send().await.expect("response").status(),
        StatusCode::OK
    );
    let _ = stop.send(());
    server.await.expect("server task");
}
