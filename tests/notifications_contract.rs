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
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::notifications::devices::{DeviceService, DeviceStore, DeviceStoreFuture};
use histae_api_rust::notifications::domain::{DevicePlatform, DeviceRecord, DeviceRegistration};
use histae_api_rust::notifications::http::{NotificationHttpState, routes};
use serde_json::{Value, json};
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
struct AuthStore {
    account: ActiveAccount,
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
        let account = self.account.clone();
        Box::pin(async move { Ok(Some(account)) })
    }
}

#[derive(Clone, Default)]
struct FakeDeviceStore {
    state: Arc<Mutex<DeviceState>>,
}

#[derive(Default)]
struct DeviceState {
    devices: Vec<DeviceRecord>,
    registration: Option<DeviceRegistration>,
    register_result: Option<DeviceRecord>,
    remove_result: bool,
    error: Option<DatabaseError>,
}

impl DeviceStore for FakeDeviceStore {
    fn register(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        registration: DeviceRegistration,
    ) -> DeviceStoreFuture<'_, Option<DeviceRecord>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.registration = Some(registration);
            if let Some(error) = state.error {
                return Err(error);
            }
            Ok(state.register_result.clone())
        })
    }

    fn list(&self, _user_id: Uuid) -> DeviceStoreFuture<'_, Vec<DeviceRecord>> {
        Box::pin(async move {
            let state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            if let Some(error) = state.error {
                return Err(error);
            }
            Ok(state.devices.clone())
        })
    }

    fn remove(&self, _user_id: Uuid, _device_id: Uuid) -> DeviceStoreFuture<'_, bool> {
        Box::pin(async move {
            let state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            if let Some(error) = state.error {
                return Err(error);
            }
            Ok(state.remove_result)
        })
    }
}

fn device() -> DeviceRecord {
    DeviceRecord {
        id: Uuid::new_v4(),
        session_id: Some(session_id()),
        platform: DevicePlatform::Android,
        app_version: Some("1.2.3".to_owned()),
        created_at: Utc
            .with_ymd_and_hms(2026, 9, 27, 12, 0, 0)
            .single()
            .expect("date"),
        last_used_at: Some(
            Utc.with_ymd_and_hms(2026, 9, 27, 12, 1, 2)
                .single()
                .expect("date"),
        ),
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

fn app(store: FakeDeviceStore, onboarded: bool) -> (axum::Router, String) {
    let account = ActiveAccount {
        user_id: user_id(),
        role: AccountRole::User,
        is_banned: false,
        onboarding_complete: onboarded,
    };
    let tokens = TokenService::new(jwt_config());
    let auth_service = MobileAuthService::new(
        tokens.clone(),
        Arc::new(AuthStore { account }),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh".to_owned(),
    ));
    let auth_state = MobileAuthState::new(
        auth_service,
        limiter.clone(),
        LimitPolicy {
            max: 30,
            window: Duration::from_secs(900),
        },
    );
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http_state = HttpState::new(
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
    .expect("HTTP state");
    let access_token = tokens
        .access_token(user_id(), session_id())
        .expect("access token");
    let service = DeviceService::new(Arc::new(store));
    (
        build_router(
            routes(NotificationHttpState::new(service), auth_state),
            http_state,
        ),
        access_token,
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
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn registers_a_device_with_the_exact_public_projection_and_pre_trim_validation() {
    let store = FakeDeviceStore::default();
    let record = device();
    store.state.lock().expect("state").register_result = Some(record.clone());
    let (app, token) = app(store.clone(), false);
    let response = app
        .oneshot(request(
            "POST",
            "/api/users/me/devices",
            Some(&token),
            r#"{"push_token":"  provider-token-with-enough-characters  ","platform":"android","app_version":" 1.2.3 "}"#,
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(response).await,
        json!({
            "id": record.id,
            "session_id": session_id(),
            "platform": "android",
            "app_version": "1.2.3",
            "created_at": "2026-09-27T12:00:00.000Z",
            "last_used_at": "2026-09-27T12:01:02.000Z"
        })
    );
    let state = store.state.lock().expect("state");
    let registration = state.registration.as_ref().expect("registration");
    assert_eq!(registration.token, "provider-token-with-enough-characters");
    assert_eq!(registration.app_version.as_deref(), Some("1.2.3"));
}

#[tokio::test]
async fn lists_and_removes_devices_without_requiring_completed_onboarding() {
    let store = FakeDeviceStore::default();
    let record = device();
    {
        let mut state = store.state.lock().expect("state");
        state.devices = vec![record.clone()];
        state.remove_result = true;
    }
    let (app, token) = app(store, false);
    let listed = app
        .clone()
        .oneshot(request("GET", "/api/users/me/devices", Some(&token), ""))
        .await
        .expect("response");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        json_body(listed).await["devices"][0]["id"],
        record.id.to_string()
    );
    let removed = app
        .oneshot(request(
            "DELETE",
            &format!("/api/users/me/devices/{}", record.id),
            Some(&token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        to_bytes(removed.into_body(), usize::MAX)
            .await
            .expect("body")
            .len(),
        0
    );
}

#[tokio::test]
async fn rejects_invalid_and_privileged_payload_fields_with_the_nest_error() {
    for body in [
        r#"{"push_token":"short","platform":"ios"}"#,
        r#"{"push_token":"provider-token-with-enough-characters","platform":"web"}"#,
        r#"{"push_token":"provider-token-with-enough-characters","platform":"ios","user_id":"forbidden"}"#,
    ] {
        let (app, token) = app(FakeDeviceStore::default(), true);
        let response = app
            .oneshot(request("POST", "/api/users/me/devices", Some(&token), body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await,
            json!({"error":{"code":"invalid_device_payload","message":"The device registration request body is invalid."}})
        );
    }
}

#[tokio::test]
async fn distinguishes_invalid_ids_unknown_devices_and_stale_sessions() {
    let store = FakeDeviceStore::default();
    let (app, token) = app(store.clone(), true);
    let invalid = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/users/me/devices/not-a-uuid",
            Some(&token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid).await["error"]["code"],
        "invalid_device_id"
    );

    let missing = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/users/me/devices/{}", Uuid::new_v4()),
            Some(&token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(missing).await["error"]["code"],
        "device_not_found"
    );

    let stale = app
        .oneshot(request(
            "POST",
            "/api/users/me/devices",
            Some(&token),
            r#"{"push_token":"provider-token-with-enough-characters","platform":"ios"}"#,
        ))
        .await
        .expect("response");
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(stale).await,
        json!({"error":{"code":"authentication_required","message":"A valid mobile session is required."}})
    );
}

#[tokio::test]
async fn authentication_and_database_failures_keep_stable_public_errors() {
    let store = FakeDeviceStore::default();
    store.state.lock().expect("state").error = Some(DatabaseError::QueryFailed);
    let (app, token) = app(store, true);
    let missing_auth = app
        .clone()
        .oneshot(request("GET", "/api/users/me/devices", None, ""))
        .await
        .expect("response");
    assert_eq!(missing_auth.status(), StatusCode::UNAUTHORIZED);
    let database = app
        .oneshot(request("GET", "/api/users/me/devices", Some(&token), ""))
        .await
        .expect("response");
    assert_eq!(database.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_body(database).await["error"]["code"], "internal_error");
}
