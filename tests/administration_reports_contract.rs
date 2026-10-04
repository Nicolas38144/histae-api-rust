use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::{TimeZone as _, Utc};
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::store::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::privacy::domain::BlockedUserRow;
use histae_api_rust::privacy::http::{PrivacyHttpState, routes as privacy_routes};
use histae_api_rust::privacy::service::{NoopPrivacyEventPublisher, PrivacyService};
use histae_api_rust::privacy::store::{PrivacyStore, PrivacyStoreFuture};
use histae_api_rust::reports::domain::{CursorReportRow, PageCursor, ReportRecord, ReportStatus};
use histae_api_rust::reports::http::{ReportHttpState, mobile_routes};
use histae_api_rust::reports::service::ReportService;
use histae_api_rust::reports::store::{ReportStore, ReportStoreFuture};
use serde_json::Value;
use tower::ServiceExt as _;
use uuid::Uuid;

fn user_id() -> Uuid {
    static ID: OnceLock<Uuid> = OnceLock::new();
    *ID.get_or_init(Uuid::new_v4)
}

fn target_id() -> Uuid {
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
struct AuthStore {
    onboarded: bool,
}

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
        let onboarded = self.onboarded;
        Box::pin(async move {
            Ok(Some(ActiveAccount {
                user_id: user_id(),
                role: AccountRole::User,
                is_banned: false,
                onboarding_complete: onboarded,
            }))
        })
    }
}

#[derive(Default)]
struct PrivacyState {
    target_exists: bool,
    blocks: Vec<BlockedUserRow>,
}

#[derive(Clone, Default)]
struct FakePrivacyStore(Arc<Mutex<PrivacyState>>);

impl PrivacyStore for FakePrivacyStore {
    fn block(&self, _blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| DatabaseError::QueryFailed)?;
            if !state.target_exists {
                return Ok(false);
            }
            if !state.blocks.iter().any(|item| item.user_id == blocked_id) {
                state.blocks.push(BlockedUserRow {
                    user_id: blocked_id,
                    firstname: Some("Blocked".to_owned()),
                    blocked_at: Utc
                        .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                        .single()
                        .ok_or(DatabaseError::QueryFailed)?,
                });
            }
            Ok(true)
        })
    }

    fn unblock(&self, _blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, ()> {
        Box::pin(async move {
            self.0
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?
                .blocks
                .retain(|item| item.user_id != blocked_id);
            Ok(())
        })
    }

    fn blocked_users(&self, _blocker_id: Uuid) -> PrivacyStoreFuture<'_, Vec<BlockedUserRow>> {
        Box::pin(async move {
            self.0
                .lock()
                .map(|state| state.blocks.clone())
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }
}

#[derive(Clone, Default)]
struct FakeReportStore(Arc<Mutex<Vec<ReportRecord>>>);

impl ReportStore for FakeReportStore {
    fn account_exists(&self, _user_id: Uuid) -> ReportStoreFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }

    fn match_participants(&self, _match_id: Uuid) -> ReportStoreFuture<'_, Option<[Uuid; 2]>> {
        Box::pin(async { Ok(Some([user_id(), target_id()])) })
    }

    fn create(&self, report: ReportRecord) -> ReportStoreFuture<'_, ()> {
        Box::pin(async move {
            self.0
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?
                .push(report);
            Ok(())
        })
    }

    fn list(
        &self,
        _status: Option<ReportStatus>,
        _limit: u32,
        _offset: u32,
        _cursor: Option<PageCursor>,
    ) -> ReportStoreFuture<'_, Vec<CursorReportRow>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn update_status(
        &self,
        _id: Uuid,
        _status: ReportStatus,
        _admin_id: Uuid,
        _admin_role: AdminRole,
    ) -> ReportStoreFuture<'_, bool> {
        Box::pin(async { Ok(false) })
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

fn fixture(
    privacy: FakePrivacyStore,
    reports: FakeReportStore,
    onboarded: bool,
) -> (axum::Router, String) {
    let tokens = TokenService::new(jwt_config());
    let auth = MobileAuthState::new(
        MobileAuthService::new(
            tokens.clone(),
            Arc::new(AuthStore { onboarded }),
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
    let routes = privacy_routes(
        PrivacyHttpState::new(PrivacyService::new(
            Arc::new(privacy),
            Arc::new(NoopPrivacyEventPublisher),
        )),
        auth.clone(),
    )
    .merge(mobile_routes(
        ReportHttpState::new(
            ReportService::new(Arc::new(reports)),
            limiter.clone(),
            LimitPolicy {
                max: 5,
                window: Duration::from_secs(3_600),
            },
        ),
        auth,
    ));
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
    .expect("HTTP state");
    (
        build_router(routes, http),
        tokens
            .access_token(user_id(), session_id())
            .expect("access token"),
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
    builder.body(Body::from(body.to_owned())).expect("request")
}

async fn json_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("bounded body");
    serde_json::from_slice(&body).expect("JSON response")
}

#[tokio::test]
async fn requires_authentication_and_completed_onboarding() {
    let (app, _) = fixture(
        FakePrivacyStore::default(),
        FakeReportStore::default(),
        true,
    );
    let response = app
        .oneshot(request("GET", "/api/users/me/blocks", None, ""))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let (app, token) = fixture(
        FakePrivacyStore::default(),
        FakeReportStore::default(),
        false,
    );
    let response = app
        .oneshot(request("GET", "/api/users/me/blocks", Some(&token), ""))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "onboarding_incomplete"
    );
}

#[tokio::test]
async fn creates_reports_with_the_exact_optional_field_contract() {
    let reports = FakeReportStore::default();
    let observed = reports.clone();
    let (app, token) = fixture(FakePrivacyStore::default(), reports, true);
    let response = app
        .oneshot(request(
            "POST",
            "/api/reports",
            Some(&token),
            &format!(
                r#"{{"reported_user_id":"{}","reason":"spam"}}"#,
                target_id()
            ),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["reported_id"], target_id().to_string());
    assert!(body.get("match_id").is_none());
    assert!(body.get("description").is_none());
    assert!(observed.0.lock().is_ok_and(|items| items.len() == 1));
}

#[tokio::test]
async fn rejects_unknown_report_fields_before_storage() {
    let reports = FakeReportStore::default();
    let observed = reports.clone();
    let (app, token) = fixture(FakePrivacyStore::default(), reports, true);
    let response = app
        .oneshot(request(
            "POST",
            "/api/reports",
            Some(&token),
            &format!(
                r#"{{"reported_user_id":"{}","reason":"spam","role":"admin"}}"#,
                target_id()
            ),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "invalid_report_payload"
    );
    assert!(observed.0.lock().is_ok_and(|items| items.is_empty()));
}

#[tokio::test]
async fn block_is_idempotent_and_the_list_never_contains_a_photo() {
    let privacy = FakePrivacyStore::default();
    if let Ok(mut state) = privacy.0.lock() {
        state.target_exists = true;
    }
    let (app, token) = fixture(privacy, FakeReportStore::default(), true);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/users/me/blocks/{}", target_id()),
            Some(&token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .oneshot(request("GET", "/api/users/me/blocks", Some(&token), ""))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["blocks"][0]["user_id"], target_id().to_string());
    assert_eq!(body["blocks"][0]["photo"], Value::Null);
}
