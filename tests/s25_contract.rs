use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, TimeDelta, Utc};
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::{MobileAuthState, routes as mobile_auth_routes};
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::privacy::erasure::{
    AcceptedErasure, AcceptedErasureStatus, AccountDeletionFuture, AccountDeletionService,
    AccountDeletionStore, NewDeletionToken,
};
use histae_api_rust::privacy::erasure_http::{AccountDeletionHttpState, routes};
use histae_api_rust::shared::clock::Clock;
use serde_json::Value;
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
        Box::pin(async move {
            Ok(Some(ActiveAccount {
                user_id: user_id(),
                role: AccountRole::User,
                is_banned: false,
                onboarding_complete: false,
            }))
        })
    }
}

#[derive(Default)]
struct DeletionState {
    account_exists: bool,
    token: Option<NewDeletionToken>,
}

#[derive(Clone, Default)]
struct DeletionStore(Arc<Mutex<DeletionState>>);

impl DeletionStore {
    fn active() -> Self {
        Self(Arc::new(Mutex::new(DeletionState {
            account_exists: true,
            token: None,
        })))
    }
}

impl AccountDeletionStore for DeletionStore {
    fn replace_token(&self, token: NewDeletionToken) -> AccountDeletionFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| DatabaseError::QueryFailed)?;
            if !state.account_exists {
                return Ok(false);
            }
            state.token = Some(token);
            Ok(true)
        })
    }

    fn accept(
        &self,
        user_id: Uuid,
        token_id: Uuid,
        token_hash: String,
        now: DateTime<Utc>,
    ) -> AccountDeletionFuture<'_, Option<AcceptedErasure>> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| DatabaseError::QueryFailed)?;
            let valid = state.token.as_ref().is_some_and(|token| {
                token.user_id == user_id
                    && token.id == token_id
                    && token.token_hash == token_hash
                    && token.expires_at > now
            });
            if !state.account_exists || !valid {
                return Ok(None);
            }
            state.token = None;
            Ok(Some(AcceptedErasure {
                request_id: Uuid::new_v4(),
                status: AcceptedErasureStatus::InProgress,
            }))
        })
    }
}

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
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

fn fixture(store: DeletionStore) -> (axum::Router, String) {
    let tokens = TokenService::new(jwt_config());
    let auth = MobileAuthState::new(
        MobileAuthService::new(
            tokens.clone(),
            Arc::new(AuthStore),
            "terms-v1".to_owned(),
            "privacy-v1".to_owned(),
        ),
        RateLimiter::memory(&SecretString::new(
            "mobile-rate-key-0123456789abcdef".to_owned(),
        )),
        LimitPolicy {
            max: 30,
            window: Duration::from_secs(900),
        },
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "global-rate-key-0123456789abcdef".to_owned(),
    ));
    let now = Utc::now();
    let service = AccountDeletionService::new(
        Arc::new(store),
        Duration::from_secs(600),
        Arc::new(FixedClock(now)),
    );
    let http = HttpState::new(
        Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe)),
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
    let route = routes(AccountDeletionHttpState::new(service), auth.clone())
        .merge(mobile_auth_routes(auth));
    (
        build_router(route, http),
        tokens
            .access_token(user_id(), session_id())
            .unwrap_or_else(|_| unreachable!()),
    )
}

fn request(method: &str, uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|_| unreachable!())
}

async fn json_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    serde_json::from_slice(&body).unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn requires_authentication_but_allows_incomplete_onboarding() {
    let (app, token) = fixture(DeletionStore::active());
    let unauthorized = app
        .clone()
        .oneshot(request("POST", "/api/users/me/deletion-token", None, ""))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let issued = app
        .oneshot(request(
            "POST",
            "/api/users/me/deletion-token",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(issued.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn issues_and_consumes_a_single_use_deletion_token() {
    let (app, token) = fixture(DeletionStore::active());
    let issued = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/users/me/deletion-token",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued = json_body(issued).await;
    let confirmation = issued["confirmation_token"]
        .as_str()
        .unwrap_or_else(|| unreachable!());
    assert_eq!(confirmation.len(), 80);
    assert!(
        issued["expires_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );

    let payload = serde_json::json!({ "confirmation_token": confirmation }).to_string();
    let accepted = app
        .clone()
        .oneshot(request("DELETE", "/api/users/me", Some(&token), &payload))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = json_body(accepted).await;
    assert_eq!(accepted["status"], "in_progress");
    assert!(
        accepted["request_id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .is_some()
    );

    let replay = app
        .oneshot(request("DELETE", "/api/users/me", Some(&token), &payload))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(replay).await["error"]["code"],
        "invalid_or_expired_deletion_token"
    );
}

#[tokio::test]
async fn rejects_invalid_unknown_and_expired_deletion_tokens() {
    let store = DeletionStore::active();
    let (app, token) = fixture(store.clone());
    let malformed = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/users/me",
            Some(&token),
            r#"{"confirmation_token":"delete-me"}"#,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(malformed).await["error"]["code"],
        "invalid_account_deletion_payload"
    );

    let unknown = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/users/me",
            Some(&token),
            &serde_json::json!({
                "confirmation_token": format!(
                    "{}:{}",
                    Uuid::new_v4(),
                    "A".repeat(43)
                ),
                "role": "superadmin"
            })
            .to_string(),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let expired_id = Uuid::new_v4();
    let expired_plain = format!("{expired_id}:{}", "B".repeat(43));
    store.0.lock().unwrap_or_else(|_| unreachable!()).token = Some(NewDeletionToken {
        id: expired_id,
        user_id: user_id(),
        token_hash: histae_api_rust::infra::crypto::sha256_hex(expired_plain.as_bytes()),
        expires_at: Utc::now() - TimeDelta::seconds(1),
    });
    let expired = app
        .oneshot(request(
            "DELETE",
            "/api/users/me",
            Some(&token),
            &serde_json::json!({ "confirmation_token": expired_plain }).to_string(),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reports_a_missing_account_when_issuing_the_token() {
    let (app, token) = fixture(DeletionStore::default());
    let response = app
        .oneshot(request(
            "POST",
            "/api/users/me/deletion-token",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "account_not_found"
    );
}
