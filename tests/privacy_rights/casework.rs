use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use histae_api_rust::casework::{Casework, routes};
use histae_api_rust::config::{AdminAuthConfig, Environment, JwtConfig, LimitPolicy, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::{HttpState, build_router};
use histae_api_rust::identity::admin::http::AdminAuthHttpState;
use histae_api_rust::identity::admin::pg::AdminAuthRepository;
use histae_api_rust::identity::admin::service::AdminAuthService;
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::pg::MobileSessionRepository;
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::TokenService;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tower::ServiceExt as _;

use super::{
    Arc, DataExportService, Database, FixtureError, PgDataExportStore, PgDataRightsStore, Photos,
    SecretString, Uuid, Value, cleanup, insert_account, local_config,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const ORIGIN: &str = "http://localhost:5173";

struct Probe;
impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

struct Client {
    router: axum::Router,
    owner: String,
    other: String,
    admin: String,
}

impl Client {
    async fn new(database: &Database, owner: Uuid, other: Uuid, admin: Uuid) -> TestResult<Self> {
        let key = SecretString::new("casework test secret for isolated PostgreSQL".into());
        let token_service = TokenService::new(JwtConfig {
            secret: key.clone(),
            active_kid: "test".into(),
            verification_keys: [("test".into(), key.clone())].into(),
            access_ttl: Duration::from_secs(600),
            refresh_ttl: Duration::from_secs(3600),
        });
        let mobile = MobileAuthService::new(
            token_service,
            Arc::new(MobileSessionRepository::new(database.clone())),
            "test".into(),
            "test".into(),
        );
        let owner = mobile.issue_token_pair(owner).await?.access_token;
        let other = mobile.issue_token_pair(other).await?.access_token;
        let config = AdminAuthConfig {
            rp_id: "localhost".into(),
            origin: ORIGIN.into(),
            rp_name: "Histae test".into(),
            challenge_ttl: Duration::from_secs(300),
            bootstrap_ttl: Duration::from_secs(900),
            session_idle_ttl: Duration::from_secs(1800),
            session_absolute_ttl: Duration::from_secs(28800),
            recent_authentication_ttl: Duration::from_secs(600),
            cookie_name: "histae_admin_session",
            secure_cookie: false,
        };
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| FixtureError("test token"))?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let credential: Uuid = sqlx::query_scalar(
            "INSERT INTO admin_webauthn_credential
             (user_id,credential_id,public_key,device_type,backed_up,name)
             VALUES ($1,$2,$3,'singleDevice',false,'Fixture') RETURNING id",
        )
        .bind(admin)
        .bind(Uuid::new_v4().simple().to_string())
        .bind(vec![1_u8])
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO admin_session
             (user_id,credential_id,token_hash,authenticated_at,idle_expires_at,absolute_expires_at)
             VALUES ($1,$2,$3,now(),now()+interval '30 minutes',now()+interval '8 hours')",
        )
        .bind(admin)
        .bind(credential)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .execute(database.acquire().await?.as_mut())
        .await?;
        let admin = AdminAuthService::new(
            Arc::new(AdminAuthRepository::new(database.clone())),
            config.clone(),
        )?;
        let limiter = RateLimiter::memory(&key);
        let limit = LimitPolicy {
            max: 100,
            window: Duration::from_secs(60),
        };
        let http = HttpState::new(
            Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe)),
            Environment::Test,
            &TrustProxy::Disabled,
            &[],
            limiter.clone(),
            limit.clone(),
        )
        .map_err(|_| FixtureError("HTTP state"))?;
        let export = DataExportService::new(
            Arc::new(PgDataExportStore::new(database.clone())),
            Arc::new(PgDataRightsStore::new(database.clone())),
            Arc::new(Photos),
            25,
            1_048_576,
            1,
        );
        Ok(Self {
            router: build_router(
                routes(
                    Casework::new(database.clone(), limiter.clone(), limit.clone(), export),
                    MobileAuthState::new(mobile, limiter.clone(), limit.clone()),
                    AdminAuthHttpState::new(admin, limiter, limit, config),
                ),
                http,
            ),
            owner,
            other,
            admin: format!("histae_admin_session={token}"),
        })
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        credential: Option<&str>,
        body: Option<Value>,
        expected: StatusCode,
    ) -> TestResult<Value> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 8000))));
        if let Some(credential) = credential {
            if path.starts_with("/api/admin/") {
                request = request
                    .header(header::COOKIE, credential)
                    .header(header::ORIGIN, ORIGIN);
            } else {
                request = request.header(header::AUTHORIZATION, format!("Bearer {credential}"));
            }
        }
        let body = if let Some(body) = body {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = self.router.clone().oneshot(request.body(body)?).await?;
        let status = response.status();
        let cache_control = response.headers().get(header::CACHE_CONTROL).cloned();
        let bytes = to_bytes(response.into_body(), 2_097_152).await?;
        assert_eq!(
            status,
            expected,
            "{method} {path}: {}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(
            cache_control.as_ref().and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        Ok(if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        })
    }
}

#[tokio::test]
async fn casework_routes_scope_reports_threads_exports_and_receipts() -> TestResult {
    let database = Database::connect(&local_config()?).await?;
    let [owner, other, admin] = std::array::from_fn(|_| Uuid::new_v4());
    let result = async {
        insert_account(&database, owner, "user").await?;
        insert_account(&database, other, "user").await?;
        insert_account(&database, admin, "admin").await?;
        let client = Client::new(&database, owner, other, admin).await?;
        let report: Uuid = sqlx::query_scalar(
            "INSERT INTO user_report(reporter_id,reported_id,reason)
             VALUES($1,$2,'other') RETURNING id",
        )
        .bind(owner)
        .bind(other)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        client
            .request(
                "PATCH",
                &format!("/api/admin/reports/{report}/decision"),
                Some(&client.admin),
                Some(json!({"status":"dismissed","response":"Report processed"})),
                StatusCode::NO_CONTENT,
            )
            .await?;
        let reports = client
            .request(
                "GET",
                "/api/users/me/reports",
                Some(&client.owner),
                None,
                StatusCode::OK,
            )
            .await?;
        assert_eq!(reports["reports"][0]["status"], "closed");
        assert_eq!(reports["reports"][0]["response"], "Report processed");
        assert!(reports["reports"][0].get("reported_id").is_none());
        let reports = client
            .request(
                "GET",
                "/api/users/me/reports",
                Some(&client.other),
                None,
                StatusCode::OK,
            )
            .await?;
        assert_eq!(reports["reports"], json!([]));
        let request: Uuid = sqlx::query_scalar(
            "INSERT INTO data_subject_request(user_id,type,status,notes)
             VALUES($1,'access','in_progress','Internal note') RETURNING id",
        )
        .bind(owner)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        let admin_thread = format!("/api/admin/data-subject-requests/{request}/messages");
        let owner_thread = format!("/api/users/me/data-subject-requests/{request}/messages");
        let summary = "Existing summary";
        sqlx::query("UPDATE data_subject_request SET response=$2 WHERE id=$1")
            .bind(request)
            .bind(summary)
            .execute(database.acquire().await?.as_mut())
            .await?;
        client
            .request(
                "POST",
                &owner_thread,
                Some(&client.owner),
                Some(json!({"message":"Additional details"})),
                StatusCode::NO_CONTENT,
            )
            .await?;
        for _ in 0..2 {
            client
                .request(
                    "POST",
                    &admin_thread,
                    Some(&client.admin),
                    Some(json!({"message":"Team message"})),
                    StatusCode::NO_CONTENT,
                )
                .await?;
        }
        let thread = client
            .request(
                "GET",
                &owner_thread,
                Some(&client.owner),
                None,
                StatusCode::OK,
            )
            .await?;
        assert_eq!(thread["messages"].as_array().map(Vec::len), Some(3));
        assert_eq!(thread["messages"][0]["body"], "Team message");
        assert!(thread.get("notes").is_none());
        let preserved: String =
            sqlx::query_scalar("SELECT response FROM data_subject_request WHERE id=$1")
                .bind(request)
                .fetch_one(database.acquire().await?.as_mut())
                .await?;
        assert_eq!(preserved, summary);
        let notifications: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM notification WHERE user_id=$1 AND type='data_request_replied'",
        )
        .bind(owner)
        .fetch_all(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0]["request_message_id"],
            thread["messages"][0]["id"]
        );
        client
            .request(
                "GET",
                &owner_thread,
                Some(&client.other),
                None,
                StatusCode::NOT_FOUND,
            )
            .await?;
        let grant_path = format!("/api/admin/data-subject-requests/{request}/assisted-export");
        let grant = client
            .request(
                "POST",
                &grant_path,
                Some(&client.admin),
                None,
                StatusCode::OK,
            )
            .await?;
        let grant_id = grant["id"].as_str().ok_or(FixtureError("grant ID"))?;
        let path = format!("/api/users/me/assisted-exports/{grant_id}");
        client
            .request(
                "GET",
                &path,
                Some(&client.other),
                None,
                StatusCode::NOT_FOUND,
            )
            .await?;
        let export = client
            .request("GET", &path, Some(&client.owner), None, StatusCode::OK)
            .await?;
        assert_eq!(
            export["data_request_messages"].as_array().map(Vec::len),
            Some(3)
        );
        assert!(!export.to_string().contains("Internal note"));
        let receipt = client
            .request(
                "POST",
                "/api/users/me/erasure-receipt",
                Some(&client.owner),
                None,
                StatusCode::OK,
            )
            .await?;
        let status = client
            .request(
                "POST",
                "/api/public/erasure-status",
                None,
                Some(json!({"code":receipt["code"]})),
                StatusCode::OK,
            )
            .await?;
        assert_eq!(status, json!({"status":"not_started","completed_at":null}));
        let cases = "/api/users/me/support-cases";
        let support = client
            .request(
                "POST",
                cases,
                Some(&client.owner),
                Some(json!({
                    "category":"other", "message":"Support needed"
                })),
                StatusCode::CREATED,
            )
            .await?;
        let support_id = support["id"].as_str().ok_or(FixtureError("support ID"))?;
        client
            .request(
                "PATCH",
                &format!("/api/admin/support-cases/{support_id}"),
                Some(&client.admin),
                Some(json!({"status":"closed","response":"Support handled"})),
                StatusCode::NO_CONTENT,
            )
            .await?;
        assert_eq!(
            client
                .request("GET", cases, Some(&client.other), None, StatusCode::OK)
                .await?["cases"],
            json!([])
        );
        let cases = client
            .request("GET", cases, Some(&client.owner), None, StatusCode::OK)
            .await?;
        assert_eq!(cases["cases"][0]["response"], "Support handled");
        assert!(cases["cases"][0].get("handled_by").is_none());
        Ok(())
    }
    .await;
    cleanup(&database, &[owner, other, admin]).await?;
    result
}
