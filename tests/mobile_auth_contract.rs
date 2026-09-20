use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::{TimeZone as _, Utc};
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::{MobileAuthState, routes};
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use uuid::Uuid;

const USER_ID: &str = "15fc0373-8ed3-4cd6-8b61-3639b84ad966";
const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

#[derive(Clone)]
struct Probe;

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct FakeStore {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Clone)]
struct FakeState {
    account: Option<ActiveAccount>,
    rotation: RotationOutcome,
    logout: bool,
    rows: Vec<MobileSessionRow>,
    revoke: Option<u64>,
}

impl FakeStore {
    fn new(account: ActiveAccount) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                account: Some(account),
                rotation: RotationOutcome::Invalid,
                logout: false,
                rows: Vec::new(),
                revoke: Some(0),
            })),
        }
    }
}

impl MobileSessionStore for FakeStore {
    fn create(
        &self,
        user_id: Uuid,
        _token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        let session_id = uuid(SESSION_ID);
        Box::pin(async move {
            Ok(Some(MobileSessionIdentity {
                user_id,
                session_id,
            }))
        })
    }

    fn rotate(
        &self,
        _jti: Uuid,
        _hash: String,
        _next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome> {
        Box::pin(async move {
            self.state
                .lock()
                .map(|state| state.rotation)
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn logout(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _jti: Uuid,
        _hash: String,
        _device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool> {
        Box::pin(async move {
            self.state
                .lock()
                .map(|state| state.logout)
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn list(
        &self,
        _user_id: Uuid,
        _limit: u32,
        _cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>> {
        Box::pin(async move {
            self.state
                .lock()
                .map(|state| state.rows.clone())
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn revoke(
        &self,
        _user_id: Uuid,
        _current_session_id: Uuid,
        _target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>> {
        Box::pin(async move {
            self.state
                .lock()
                .map(|state| state.revoke)
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn active_account(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _terms_version: Arc<str>,
        _privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>> {
        Box::pin(async move {
            self.state
                .lock()
                .map(|state| state.account.clone())
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }
}

fn uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("valid fixture UUID")
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

fn account() -> ActiveAccount {
    ActiveAccount {
        user_id: uuid(USER_ID),
        role: AccountRole::User,
        is_banned: false,
        onboarding_complete: false,
    }
}

fn app(store: FakeStore) -> (Router, TokenService) {
    let tokens = TokenService::new(jwt_config());
    let service = MobileAuthService::new(
        tokens.clone(),
        Arc::new(store),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let rate_key = SecretString::new("hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh".to_owned());
    let limiter = RateLimiter::memory(&rate_key);
    let policy = LimitPolicy {
        max: 30,
        window: Duration::from_secs(900),
    };
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http_state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter.clone(),
        LimitPolicy {
            max: 100,
            window: Duration::from_secs(60),
        },
    )
    .expect("valid HTTP state");
    (
        build_router(
            routes(MobileAuthState::new(service, limiter, policy)),
            http_state,
        ),
        tokens,
    )
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

fn request(method: &str, uri: &str, bearer: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

#[tokio::test]
async fn me_rechecks_the_session_and_preserves_authentication_errors() {
    let store = FakeStore::new(account());
    let (app, tokens) = app(store.clone());
    let missing = app
        .clone()
        .oneshot(request("GET", "/api/auth/me", None, ""))
        .await
        .expect("response");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(missing).await["error"]["code"],
        "authentication_required"
    );

    let invalid = app
        .clone()
        .oneshot(request("GET", "/api/auth/me", Some("invalid"), ""))
        .await
        .expect("response");
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(invalid).await["error"]["code"],
        "invalid_or_expired_access_token"
    );

    let access = tokens
        .access_token(uuid(USER_ID), uuid(SESSION_ID))
        .expect("access token");
    let response = app
        .clone()
        .oneshot(request("GET", "/api/auth/me", Some(&access), ""))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        json!({ "user_id": USER_ID, "onboarding_complete": false })
    );

    store.state.lock().expect("state").account = None;
    let revoked = app
        .clone()
        .oneshot(request("GET", "/api/auth/me", Some(&access), ""))
        .await
        .expect("response");
    assert_eq!(
        json_body(revoked).await["error"]["code"],
        "authentication_required"
    );

    let mut banned_account = account();
    banned_account.is_banned = true;
    store.state.lock().expect("state").account = Some(banned_account);
    let banned = app
        .oneshot(request("GET", "/api/auth/me", Some(&access), ""))
        .await
        .expect("response");
    assert_eq!(banned.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(banned).await["error"]["code"],
        "account_unavailable"
    );
}

#[tokio::test]
async fn auth_precedes_body_validation_and_refresh_is_strict() {
    let store = FakeStore::new(account());
    let (app, tokens) = app(store);
    let unauthenticated = app
        .clone()
        .oneshot(request("POST", "/api/auth/logout", None, "{}"))
        .await
        .expect("response");
    assert_eq!(
        json_body(unauthenticated).await["error"]["code"],
        "authentication_required"
    );

    let access = tokens
        .access_token(uuid(USER_ID), uuid(SESSION_ID))
        .expect("access token");
    let bad_logout = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/auth/logout",
            Some(&access),
            r#"{"refresh_token":"short","admin":true}"#,
        ))
        .await
        .expect("response");
    assert_eq!(bad_logout.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(bad_logout).await["error"]["code"],
        "invalid_request_body"
    );

    for body in [
        r#"{"refresh_token":"short"}"#,
        r#"{"refresh_token":"short","unknown":true}"#,
        &format!(r#"{{"refresh_token":"{}"}}"#, "a".repeat(129)),
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", "/api/auth/refresh", None, body))
            .await
            .expect("response");
        let payload = json_body(response).await;
        assert!(matches!(
            payload["error"]["code"].as_str(),
            Some("invalid_or_expired_refresh_token" | "invalid_request_body")
        ));
    }
}

#[tokio::test]
async fn session_pagination_dates_and_errors_match_the_nest_contract() {
    let store = FakeStore::new(account());
    let first = uuid(SESSION_ID);
    let second = uuid("22222222-2222-4222-8222-222222222222");
    let third = uuid("33333333-3333-4333-8333-333333333333");
    let at = Utc
        .with_ymd_and_hms(2026, 9, 20, 12, 34, 56)
        .single()
        .expect("timestamp")
        + chrono::Duration::microseconds(123_456);
    store.state.lock().expect("state").rows = vec![
        MobileSessionRow {
            id: first,
            created_at: at,
            last_refreshed_at: at,
            expires_at: at,
            cursor_at: "2026-09-20T12:34:56.123456Z".to_owned(),
        },
        MobileSessionRow {
            id: second,
            created_at: at,
            last_refreshed_at: at,
            expires_at: at,
            cursor_at: "2026-09-20T12:34:56.123456Z".to_owned(),
        },
        MobileSessionRow {
            id: third,
            created_at: at,
            last_refreshed_at: at,
            expires_at: at,
            cursor_at: "2026-09-20T12:34:56.123456Z".to_owned(),
        },
    ];
    let (app, tokens) = app(store.clone());
    let access = tokens
        .access_token(uuid(USER_ID), first)
        .expect("access token");
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/auth/sessions?limit=2",
            Some(&access),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = json_body(response).await;
    assert_eq!(payload["sessions"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["sessions"][0]["current"], true);
    assert_eq!(
        payload["sessions"][0]["created_at"],
        "2026-09-20T12:34:56.123Z"
    );
    assert!(payload["next_cursor"].is_string());

    let invalid_query = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/auth/sessions?limit=0",
            Some(&access),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(invalid_query).await["error"]["code"],
        "invalid_session_query"
    );
    let invalid_cursor = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/auth/sessions?cursor=invalid",
            Some(&access),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(invalid_cursor).await["error"]["code"],
        "invalid_cursor"
    );

    let bad_id = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/auth/sessions/11111111-1111-1111-8111-111111111111",
            Some(&access),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(bad_id).await["error"]["code"],
        "invalid_session_id"
    );

    store.state.lock().expect("state").revoke = Some(0);
    let absent = app
        .oneshot(request(
            "DELETE",
            "/api/auth/sessions/22222222-2222-4222-8222-222222222222",
            Some(&access),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(absent).await["error"]["code"],
        "session_not_found"
    );
}

#[tokio::test]
async fn logout_all_requires_literal_true_and_returns_the_revoked_count() {
    let store = FakeStore::new(account());
    store.state.lock().expect("state").revoke = Some(3);
    let (app, tokens) = app(store);
    let access = tokens
        .access_token(uuid(USER_ID), uuid(SESSION_ID))
        .expect("access token");
    let rejected = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/auth/logout-all",
            Some(&access),
            r#"{"confirm":false}"#,
        ))
        .await
        .expect("response");
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let response = app
        .oneshot(request(
            "POST",
            "/api/auth/logout-all",
            Some(&access),
            r#"{"confirm":true}"#,
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, json!({ "revoked_sessions": 3 }));
}
