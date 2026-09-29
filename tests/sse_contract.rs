use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use futures_util::StreamExt as _;
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::redis::RedisService;
use histae_api_rust::notifications::sse::{
    RealtimeService, SessionActivity, SessionActivityFuture, SseHttpState, routes,
};
use serde_json::Value;
use tokio::time;
use tower::ServiceExt as _;
use uuid::Uuid;

fn user_id() -> Uuid {
    static ID: OnceLock<Uuid> = OnceLock::new();
    *ID.get_or_init(Uuid::new_v4)
}

fn session_id() -> Uuid {
    static ID: OnceLock<Uuid> = OnceLock::new();
    *ID.get_or_init(Uuid::new_v4)
}

#[derive(Clone)]
struct Probe;

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct AuthStore;

impl MobileSessionStore for AuthStore {
    fn create(
        &self,
        user_id: Uuid,
        _token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        Box::pin(async move {
            Ok(Some(MobileSessionIdentity {
                user_id,
                session_id: session_id(),
            }))
        })
    }

    fn rotate(
        &self,
        _jti: Uuid,
        _hash: String,
        _next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome> {
        Box::pin(async { Ok(RotationOutcome::Invalid) })
    }

    fn logout(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _jti: Uuid,
        _hash: String,
        _device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn list(
        &self,
        _user_id: Uuid,
        _limit: u32,
        _cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn revoke(
        &self,
        _user_id: Uuid,
        _current_session_id: Uuid,
        _target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>> {
        Box::pin(async { Ok(Some(0)) })
    }

    fn active_account(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _terms_version: Arc<str>,
        _privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>> {
        Box::pin(async {
            Ok(Some(ActiveAccount {
                user_id: user_id(),
                role: AccountRole::User,
                is_banned: false,
                onboarding_complete: true,
            }))
        })
    }
}

struct ActiveSession;

impl SessionActivity for ActiveSession {
    fn is_active(&self, _user_id: Uuid, _session_id: Uuid) -> SessionActivityFuture<'_> {
        Box::pin(async { Ok(true) })
    }
}

fn jwt_config() -> JwtConfig {
    let secret = SecretString::new("jwt-signing-secret-0123456789abcdef".to_owned());
    JwtConfig {
        secret: secret.clone(),
        active_kid: "primary".to_owned(),
        verification_keys: BTreeMap::from([("primary".to_owned(), secret)]),
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(3_600),
    }
}

async fn app() -> (axum::Router, String) {
    let tokens = TokenService::new(jwt_config());
    let auth_service = MobileAuthService::new(
        tokens.clone(),
        Arc::new(AuthStore),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "rate-limit-secret-0123456789abcdef".to_owned(),
    ));
    let auth = MobileAuthState::new(
        auth_service,
        limiter.clone(),
        LimitPolicy {
            max: 30,
            window: Duration::from_secs(900),
        },
    );
    let realtime = RealtimeService::connect(RedisService::disabled())
        .await
        .unwrap_or_else(|_| unreachable!());
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter,
        LimitPolicy {
            max: 100,
            window: Duration::from_secs(60),
        },
    )
    .unwrap_or_else(|_| unreachable!());
    let token = tokens
        .access_token(user_id(), session_id())
        .unwrap_or_else(|_| unreachable!());
    (
        build_router(
            routes(SseHttpState::new(realtime, Arc::new(ActiveSession)), auth),
            http,
        ),
        token,
    )
}

fn request(token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/api/users/me/events");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(Body::empty())
        .unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn requires_mobile_authentication() {
    let (app, _) = app().await;
    let response = app
        .oneshot(request(None))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| unreachable!());
    assert_eq!(body["error"]["code"], "authentication_required");
}

#[tokio::test]
async fn opens_the_nest_compatible_event_stream_with_an_initial_connected_event() {
    let (app, token) = app().await;
    let response = app
        .oneshot(request(Some(&token)))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let mut body = response.into_body().into_data_stream();
    let chunk = time::timeout(Duration::from_secs(1), body.next())
        .await
        .unwrap_or_else(|_| unreachable!())
        .unwrap_or_else(|| unreachable!())
        .unwrap_or_else(|_| unreachable!());
    let text = std::str::from_utf8(&chunk).unwrap_or_else(|_| unreachable!());
    assert!(text.contains("event: connected"));
    assert!(text.contains("server_time"));
    assert!(!text.contains(&user_id().to_string()));
}
