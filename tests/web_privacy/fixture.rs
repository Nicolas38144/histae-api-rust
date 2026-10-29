use std::{
    env, fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use histae_api_rust::{
    billing::{
        pg::PgBillingRepository,
        service::BillingService,
        stripe::StripeClient,
        web::{
            WebBillingRepository, WebBillingService,
            http::{WebBillingState, routes as billing_routes},
        },
    },
    catalog::{pg::PgCatalogRepository, service::CatalogService},
    config::{
        BillingConfig, BillingProvider, Environment, LegalConfig, LimitPolicy, PostgresConfig,
        SecretString, TrustProxy, WebBillingConfig,
    },
    http::{
        health::{DependencyProbe, ProbeFuture, Readiness},
        rate_limit::RateLimiter,
        router::{HttpState, build_router},
    },
    identity::mobile::{
        otp::{OtpRepository, OtpService},
        sweego::{SmsDelivery, SmsDeliveryFuture, SmsDeliveryReceipt, SmsMessage},
    },
    infra::{crypto::sha256_hex, postgres::Database, postgres_locks::AccountActivityPool},
    media::{
        codec::PhotoCodecError,
        pg::PgPhotoRepository,
        service::{PhotoProcessor, PhotoService, ProcessorFuture},
        storage::{ObjectStorageError, PhotoObjectStorage, StorageFuture},
    },
    outbox::pg::PgOutboxRepository,
    privacy::{
        erasure::{AccountDeletionService, pg::PgErasureRepository},
        web::{WebDataDeletionService, WebPrivacyState, routes},
    },
    profiles::{pg::PgProfileRepository, service::ProfileService},
    shared::clock::SystemClock,
};
use serde_json::Value;
use tower::ServiceExt as _;
use url::Url;
use uuid::Uuid;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
pub const ORIGIN: &str = "https://www.histae.test";
pub const PHONE: &str = "+33600000002";

#[derive(Debug)]
struct FixtureError(&'static str);
impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Web privacy fixture error ({})", self.0)
    }
}
impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
    let _ = dotenvy::dotenv();
    env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or(FixtureError(name))
}

fn postgres_config() -> Result<PostgresConfig, FixtureError> {
    if variable("ENV")? != "development" || variable("POSTGRES_DB")? != "histae-dev" {
        return Err(FixtureError("development database required"));
    }
    let host = variable("POSTGRES_HOST")?;
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(FixtureError("loopback database required"));
    }
    Ok(PostgresConfig {
        host,
        port: variable("POSTGRES_PORT")?
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        database: "histae-dev".into(),
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        tls: false,
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-web-privacy-test",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

fn legal() -> LegalConfig {
    LegalConfig {
        document_dates: Default::default(),
        terms_version: "web-terms-v1".into(),
        privacy_version: "web-privacy-v1".into(),
        sensitive_data_consent_version: "web-sensitive-v1".into(),
        location_consent_version: "web-location-v1".into(),
        terms_url: Url::parse("https://www.histae.test/legal/terms").unwrap(),
        privacy_url: Url::parse("https://www.histae.test/legal/privacy").unwrap(),
        sensitive_data_consent_url: Url::parse("https://www.histae.test/legal/sensitive").unwrap(),
        location_consent_url: Url::parse("https://www.histae.test/legal/location").unwrap(),
        review_reference: "test".into(),
    }
}

fn billing_config() -> BillingConfig {
    BillingConfig {
        provider: BillingProvider::Disabled,
        stripe_secret_key: SecretString::new(String::new()),
        stripe_webhook_secret: SecretString::new(String::new()),
        premium_monthly_price_id: String::new(),
        premium_annual_price_id: String::new(),
        checkout_success_url: None,
        checkout_cancel_url: None,
        portal_return_url: None,
        automatic_tax: false,
        allow_promotion_codes: false,
        timeout: Duration::from_secs(1),
        max_network_retries: 0,
        reconciliation_interval: Duration::from_secs(300),
        reconciliation_freshness: Duration::from_secs(3600),
        reconciliation_batch_size: 25,
    }
}

#[derive(Default)]
pub struct SyntheticSms(pub Mutex<Vec<String>>);
impl SmsDelivery for SyntheticSms {
    fn send_otp(&self, message: SmsMessage) -> SmsDeliveryFuture<'_> {
        self.0.lock().unwrap().push(message.code);
        Box::pin(async {
            Ok(SmsDeliveryReceipt {
                transaction_id: Uuid::new_v4().to_string(),
                message_id: Uuid::new_v4().to_string(),
            })
        })
    }
}

struct UnusedProcessor;
impl PhotoProcessor for UnusedProcessor {
    fn process<'a>(
        &'a self,
        _: &'a histae_api_rust::media::domain::UploadPhoto,
    ) -> ProcessorFuture<'a> {
        Box::pin(async { Err(PhotoCodecError::CodecUnavailable) })
    }
}
struct UnusedStorage;
impl PhotoObjectStorage for UnusedStorage {
    fn put<'a>(
        &'a self,
        _: &'a str,
        _: Vec<u8>,
        _: &'static str,
        _: &'static str,
    ) -> StorageFuture<'a, ()> {
        Box::pin(async { Err(ObjectStorageError) })
    }
    fn delete<'a>(&'a self, _: &'a str) -> StorageFuture<'a, ()> {
        Box::pin(async { Err(ObjectStorageError) })
    }
    fn signed_get_url<'a>(&'a self, _: &'a str, _: u32) -> StorageFuture<'a, String> {
        Box::pin(async { Err(ObjectStorageError) })
    }
    fn check(&self) -> StorageFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Probe;
impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

pub struct Fixture {
    pub db: Database,
    pub app: Router,
    pub user: Uuid,
    pub other: Uuid,
    pub cookie: String,
    pub other_cookie: String,
    pub sms: Arc<SyntheticSms>,
    activity: AccountActivityPool,
    phone_hash: String,
}

fn policy(max: u64) -> LimitPolicy {
    LimitPolicy {
        max,
        window: Duration::from_secs(60),
    }
}

impl Fixture {
    pub async fn new(privacy_max: u64, otp_max: u64) -> TestResult<Self> {
        let config = postgres_config()?;
        let db = Database::connect(&config).await?;
        let activity = AccountActivityPool::connect(&config).await?;
        let clock = Arc::new(SystemClock);
        let sms = Arc::new(SyntheticSms::default());
        let key = SecretString::new(sha256_hex(Uuid::new_v4().as_bytes()));
        let otp = OtpService::new(
            Arc::new(OtpRepository::new(db.clone())),
            sms.clone(),
            key.clone(),
            "fr".into(),
            Duration::from_secs(1),
            Duration::from_secs(300),
        );
        let billing_config = billing_config();
        let billing = Arc::new(BillingService::new(
            Arc::new(PgBillingRepository::new(db.clone())),
            Arc::new(StripeClient::with_endpoint(
                billing_config.clone(),
                Url::parse("http://127.0.0.1:9/")?,
            )?),
            billing_config,
            activity.clone(),
            clock.clone(),
        ));
        let auth =
            WebBillingService::new(WebBillingRepository::new(db.clone(), legal()), otp, billing);
        let phone_hash = auth
            .phone_key(PHONE)
            .map_err(|_| FixtureError("phone hash"))?;
        let limiter = RateLimiter::memory(&key);
        let photos = PhotoService::new(
            Arc::new(PgPhotoRepository::new(
                db.clone(),
                PgOutboxRepository::new(db.clone()),
            )),
            Arc::new(UnusedProcessor),
            Arc::new(UnusedStorage),
            activity.clone(),
            clock.clone(),
        );
        let profiles = ProfileService::new(
            Arc::new(PgProfileRepository::new(db.clone())),
            legal(),
            clock.clone(),
            Arc::new(photos.clone()),
        );
        let web_config = WebBillingConfig {
            enabled: false,
            origin: ORIGIN.into(),
            secure_cookie: true,
        };
        let privacy = WebPrivacyState {
            auth: auth.clone(),
            config: web_config.clone(),
            limiter: limiter.clone(),
            policy: policy(privacy_max),
            deletion: AccountDeletionService::new(
                Arc::new(PgErasureRepository::new(db.clone())),
                Duration::from_secs(300),
                clock,
            ),
            data: WebDataDeletionService::new(photos, profiles),
        };
        let web = WebBillingState {
            service: auth,
            catalog: CatalogService::new(Arc::new(PgCatalogRepository::new(db.clone()))),
            config: web_config,
            limiter: limiter.clone(),
            otp_policy: policy(otp_max),
            billing_policy: policy(100),
        };
        let http = HttpState::new(
            Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe)),
            Environment::Test,
            &TrustProxy::Disabled,
            &[],
            limiter,
            policy(1000),
        )
        .unwrap();
        let app = build_router(routes(privacy).merge(billing_routes(web)), http);
        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut cookies = Vec::new();
        for id in [user, other] {
            sqlx::query("INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted) VALUES($1,$2,''::bytea)")
                .bind(id).bind(if id == user { phone_hash.clone() } else { format!("web-privacy-{id}") }).execute(&mut *db.acquire().await?).await?;
            let token = URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes().repeat(2));
            sqlx::query("INSERT INTO web_billing_session(user_id,token_hash) VALUES($1,$2)")
                .bind(id)
                .bind(sha256_hex(token.as_bytes()))
                .execute(&mut *db.acquire().await?)
                .await?;
            cookies.push(format!("__Host-histae_billing={token}"));
        }
        Ok(Self {
            db,
            app,
            user,
            other,
            cookie: cookies.remove(0),
            other_cookie: cookies.remove(0),
            sms,
            activity,
            phone_hash,
        })
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: &str,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ORIGIN, ORIGIN)
            .header(header::USER_AGENT, "web-privacy-test")
            .header(header::ACCEPT_LANGUAGE, "it");
        if !body.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    pub async fn send(&self, request: Request<Body>) -> TestResult<(StatusCode, HeaderMap, Value)> {
        let response = self.app.clone().oneshot(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, headers, body))
    }

    pub async fn cleanup(self) -> TestResult {
        let users = [self.user, self.other];
        sqlx::query("DELETE FROM outbox_event WHERE (event_type='account.erase' AND aggregate_id IN (SELECT id FROM data_subject_request WHERE user_id=ANY($1::uuid[]))) OR (event_type='photo.delete' AND aggregate_id IN (SELECT id FROM user_photo WHERE user_id=ANY($1::uuid[])))").bind(users.as_slice()).execute(&mut *self.db.acquire().await?).await?;
        sqlx::query("DELETE FROM data_access_log WHERE accessed_user_id=ANY($1::uuid[])")
            .bind(users.as_slice())
            .execute(&mut *self.db.acquire().await?)
            .await?;
        sqlx::query("DELETE FROM web_billing_challenge WHERE requested_user_id=ANY($1::uuid[])")
            .bind(users.as_slice())
            .execute(&mut *self.db.acquire().await?)
            .await?;
        sqlx::query("DELETE FROM user_account WHERE user_id=ANY($1::uuid[])")
            .bind(users.as_slice())
            .execute(&mut *self.db.acquire().await?)
            .await?;
        sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash=$1")
            .bind(&self.phone_hash)
            .execute(&mut *self.db.acquire().await?)
            .await?;
        self.activity.close().await;
        self.db.close().await;
        Ok(())
    }
}
