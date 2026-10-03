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
use histae_api_rust::privacy::export::{
    DataExportFuture, DataExportService, DataExportStore, ExportSnapshot, JsonExportWriter,
};
use histae_api_rust::privacy::export_http::{DataExportHttpState, routes as export_routes};
use histae_api_rust::privacy::rights::{
    AdminDataRequestRow, DataAccessLogRow, DataRequestRow, DataRequestStatus, DataRequestType,
    DataRightsFuture, DataRightsService, DataRightsStore, PageCursor, UpdateRequestInput,
    UpdateRequestResult,
};
use histae_api_rust::privacy::rights_http::{
    DataRightsHttpState, mobile_routes as data_rights_routes,
};
use histae_api_rust::profiles::service::{ProfilePhotoUrlFuture, ProfilePhotoUrlProvider};
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
struct RightsState {
    requests: Vec<DataRequestRow>,
    exports: u32,
}

#[derive(Clone, Default)]
struct RightsStore(Arc<Mutex<RightsState>>);

impl DataRightsStore for RightsStore {
    fn create_request(
        &self,
        user_id: Uuid,
        request_type: DataRequestType,
    ) -> DataRightsFuture<'_, Option<DataRequestRow>> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| DatabaseError::QueryFailed)?;
            if state.requests.iter().any(|request| {
                request.request_type == request_type
                    && matches!(
                        request.status,
                        DataRequestStatus::Pending | DataRequestStatus::InProgress
                    )
            }) {
                return Ok(None);
            }
            let row = DataRequestRow {
                id: Uuid::new_v4(),
                user_id,
                request_type,
                status: DataRequestStatus::Pending,
                requested_at: Utc
                    .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                    .single()
                    .ok_or(DatabaseError::QueryFailed)?,
                completed_at: None,
                handled_by: None,
            };
            state.requests.push(row.clone());
            Ok(Some(row))
        })
    }

    fn requests_for_user(&self, _user_id: Uuid) -> DataRightsFuture<'_, Vec<DataRequestRow>> {
        Box::pin(async move {
            self.0
                .lock()
                .map(|state| state.requests.clone())
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn requests_for_admin(
        &self,
        _status: Option<DataRequestStatus>,
        _limit: u32,
        _offset: u32,
        _cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<AdminDataRequestRow>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn update_request(
        &self,
        _input: UpdateRequestInput,
    ) -> DataRightsFuture<'_, UpdateRequestResult> {
        Box::pin(async { Ok(UpdateRequestResult::Updated) })
    }

    fn access_logs(
        &self,
        _user_id: Uuid,
        _limit: u32,
        _offset: u32,
        _cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<DataAccessLogRow>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn record_self_export(&self, _user_id: Uuid) -> DataRightsFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.exports = state.exports.saturating_add(1);
            Ok(())
        })
    }
}

#[derive(Clone)]
struct ExportStore {
    oversized: bool,
}

impl DataExportStore for ExportStore {
    fn write_snapshot<'a>(
        &'a self,
        user_id: Uuid,
        writer: &'a mut JsonExportWriter,
        _page_size: u32,
    ) -> DataExportFuture<'a> {
        Box::pin(async move {
            writer.start_array("traits").await?;
            if self.oversized {
                writer.item(&"x".repeat(4_096)).await?;
            } else {
                writer
                    .item(&json!({"id": Uuid::new_v4(), "name": "Hiking"}))
                    .await?;
            }
            writer.end_array().await?;
            Ok(ExportSnapshot {
                snapshot_at: Utc
                    .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                    .single()
                    .ok_or(DatabaseError::QueryFailed)?,
                account: json!({"user_id": user_id}),
                profile: json!({"firstname": "Ada"}),
                photo_key: None,
                preferences: Value::Null,
                subscription: Value::Null,
                discovery_rows: 0,
            })
        })
    }
}

#[derive(Clone)]
struct Photos;

impl ProfilePhotoUrlProvider for Photos {
    fn url_for_key(&self, _object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async { Ok(None) })
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
    rights: RightsStore,
    onboarded: bool,
    oversized: bool,
    max_bytes: u64,
    max_concurrency: u8,
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
    let rights_service = DataRightsService::new(Arc::new(rights.clone()));
    let export_service = DataExportService::new(
        Arc::new(ExportStore { oversized }),
        Arc::new(rights),
        Arc::new(Photos),
        25,
        max_bytes,
        max_concurrency,
    );
    let routes = data_rights_routes(DataRightsHttpState::new(rights_service), auth.clone()).merge(
        export_routes(
            DataExportHttpState::new(
                export_service,
                limiter.clone(),
                LimitPolicy {
                    max: 10,
                    window: Duration::from_secs(3_600),
                },
            ),
            auth,
        ),
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
    (
        build_router(routes, http),
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
    let body = to_bytes(response.into_body(), 256 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    serde_json::from_slice(&body).unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn data_rights_require_authentication_but_allow_incomplete_onboarding() {
    let rights = RightsStore::default();
    let (app, token) = fixture(rights, false, false, 64 * 1024, 1);
    let unauthorized = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me/data-subject-requests",
            None,
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/users/me/data-subject-requests",
            Some(&token),
            r#"{"type":"access"}"#,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(json_body(created).await["type"], "access");

    let export = app
        .oneshot(request(
            "GET",
            "/api/users/me/data-export",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(export.status(), StatusCode::OK);
}

#[tokio::test]
async fn creates_lists_and_rejects_duplicate_or_unknown_data_requests() {
    let rights = RightsStore::default();
    let (app, token) = fixture(rights, true, false, 64 * 1024, 1);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/users/me/data-subject-requests",
            Some(&token),
            r#"{"type":"portability"}"#,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(created.status(), StatusCode::CREATED);
    let duplicate = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/users/me/data-subject-requests",
            Some(&token),
            r#"{"type":"portability"}"#,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(duplicate).await["error"]["code"],
        "data_request_already_open"
    );
    let unknown = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/users/me/data-subject-requests",
            Some(&token),
            r#"{"type":"access","role":"superadmin"}"#,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/users/me/data-subject-requests",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    let json = json_body(listed).await;
    assert_eq!(json["requests"].as_array().map(Vec::len), Some(1));
    assert!(json["requests"][0].get("notes").is_none());
}

#[tokio::test]
async fn export_is_an_attachment_and_holds_its_slot_until_the_body_closes() {
    let rights = RightsStore::default();
    let (app, token) = fixture(rights.clone(), true, false, 64 * 1024, 1);
    let first = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me/data-export",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.headers()[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    assert_eq!(
        first.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"histae-data-export.json\""
    );

    let busy = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me/data-export",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()[header::RETRY_AFTER], "30");
    assert_eq!(json_body(busy).await["error"]["code"], "data_export_busy");

    let body = json_body(first).await;
    assert_eq!(body["account"]["user_id"], user_id().to_string());
    assert_eq!(body["profile"]["photo"], Value::Null);
    assert_eq!(body["consistency"]["postgres"]["level"], "repeatable_read");
    assert_eq!(
        rights
            .0
            .lock()
            .map(|state| state.exports)
            .unwrap_or_default(),
        1
    );
}

#[tokio::test]
async fn oversized_exports_fail_before_exposing_a_partial_response() {
    let (app, token) = fixture(RightsStore::default(), true, true, 1_024, 1);
    let response = app
        .oneshot(request(
            "GET",
            "/api/users/me/data-export",
            Some(&token),
            "",
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "data_export_too_large"
    );
}
