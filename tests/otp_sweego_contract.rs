use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use histae_api_rust::config::{
    Environment, JwtConfig, LimitPolicy, SecretString, SmsConfig, SmsProvider, TrustProxy,
};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::account::{
    AccountStoreError, AccountStoreFuture, MobileAccount, MobileAccountStore, MobileLoginService,
    NewMobileAccount,
};
use histae_api_rust::identity::mobile::domain::{
    ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome, SessionCursor,
};
use histae_api_rust::identity::mobile::http::{
    OtpHttpState, SweegoHttpState, otp_routes, sweego_routes,
};
use histae_api_rust::identity::mobile::otp::{
    BeginOtpDelivery, OtpDeliverySnapshot, OtpDeliveryStart, OtpDeliveryState, OtpDeliveryStates,
    OtpService, OtpStore, OtpStoreFuture, SmsDeliveryEvent, SmsEventOutcome,
};
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::sweego::{
    SmsDelivery, SmsDeliveryError, SmsDeliveryFuture, SmsDeliveryReceipt, SmsFailureReason,
    SmsMessage, SweegoWebhookMetrics, SweegoWebhookService,
};
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use hmac::{Hmac, Mac as _};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt as _;
use uuid::Uuid;

const PHONE: &str = "+33612345678";

fn idempotency_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| Uuid::new_v4().to_string())
}

#[derive(Clone)]
struct Probe;

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Debug)]
struct Attempt {
    id: Uuid,
    phone_hash: String,
    otp_hash: String,
    idempotency_key: Uuid,
    state: OtpDeliveryState,
    used: bool,
}

#[derive(Clone)]
struct FakeOtpStore {
    attempts: Arc<Mutex<Vec<Attempt>>>,
    webhook_outcome: Arc<Mutex<SmsEventOutcome>>,
}

impl Default for FakeOtpStore {
    fn default() -> Self {
        Self {
            attempts: Arc::new(Mutex::new(Vec::new())),
            webhook_outcome: Arc::new(Mutex::new(SmsEventOutcome::Applied)),
        }
    }
}

impl FakeOtpStore {
    fn attempt_count(&self) -> usize {
        self.attempts.lock().map_or(0, |attempts| attempts.len())
    }
}

impl OtpStore for FakeOtpStore {
    fn begin(&self, input: BeginOtpDelivery) -> OtpStoreFuture<'_, OtpDeliveryStart> {
        Box::pin(async move {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?;
            if let Some(existing) = attempts
                .iter()
                .find(|attempt| attempt.idempotency_key == input.idempotency_key)
            {
                if existing.phone_hash != input.phone_hash {
                    return Ok(OtpDeliveryStart::Conflict);
                }
                return Ok(OtpDeliveryStart::Existing(existing.state, existing.id));
            }
            attempts.push(Attempt {
                id: input.id,
                phone_hash: input.phone_hash,
                otp_hash: input.otp_hash,
                idempotency_key: input.idempotency_key,
                state: OtpDeliveryState::Pending,
                used: false,
            });
            Ok(OtpDeliveryStart::Created(input.id))
        })
    }

    fn mark_accepted(
        &self,
        id: Uuid,
        phone_hash: String,
        _transaction_id: String,
        _message_id: String,
    ) -> OtpStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?;
            let Some(attempt) = attempts
                .iter_mut()
                .find(|attempt| attempt.id == id && attempt.phone_hash == phone_hash)
            else {
                return Ok(false);
            };
            attempt.state = OtpDeliveryState::Accepted;
            Ok(true)
        })
    }

    fn mark_outcome(
        &self,
        id: Uuid,
        phone_hash: String,
        state: OtpDeliveryState,
        _reason: SmsFailureReason,
    ) -> OtpStoreFuture<'_, OtpDeliveryState> {
        Box::pin(async move {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?;
            if let Some(attempt) = attempts
                .iter_mut()
                .find(|attempt| attempt.id == id && attempt.phone_hash == phone_hash)
            {
                attempt.state = state;
            }
            Ok(state)
        })
    }

    fn apply_sms_event(&self, _event: SmsDeliveryEvent) -> OtpStoreFuture<'_, SmsEventOutcome> {
        Box::pin(async move {
            self.webhook_outcome
                .lock()
                .map(|outcome| *outcome)
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }

    fn consume(&self, phone_hash: String, otp_hash: String) -> OtpStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| DatabaseError::QueryFailed)?;
            let Some(attempt) = attempts.iter_mut().find(|attempt| {
                attempt.phone_hash == phone_hash
                    && attempt.otp_hash == otp_hash
                    && matches!(
                        attempt.state,
                        OtpDeliveryState::Accepted | OtpDeliveryState::Sent
                    )
                    && !attempt.used
            }) else {
                return Ok(false);
            };
            attempt.used = true;
            Ok(true)
        })
    }

    fn snapshot(&self) -> OtpStoreFuture<'_, OtpDeliverySnapshot> {
        Box::pin(async {
            Ok(OtpDeliverySnapshot {
                states: OtpDeliveryStates {
                    pending: 0,
                    accepted: 0,
                    sent: 0,
                    failed: 0,
                    unknown: 0,
                },
                awaiting_callback: 0,
                oldest_unresolved_age_seconds: None,
                average_acceptance_ms: None,
                average_sent_callback_ms: None,
                average_failure_ms: None,
                retention: "otp_expiry",
                handset_delivery: "not_confirmed",
            })
        })
    }
}

#[derive(Clone)]
struct FakeDelivery {
    messages: Arc<Mutex<Vec<SmsMessage>>>,
    result: Arc<Mutex<Result<SmsDeliveryReceipt, SmsDeliveryError>>>,
}

impl Default for FakeDelivery {
    fn default() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
            result: Arc::new(Mutex::new(Ok(SmsDeliveryReceipt {
                transaction_id: "transaction-1".to_owned(),
                message_id: "message-1".to_owned(),
            }))),
        }
    }
}

impl FakeDelivery {
    fn last_code(&self) -> Option<String> {
        self.messages
            .lock()
            .ok()
            .and_then(|messages| messages.last().map(|message| message.code.clone()))
    }

    fn calls(&self) -> usize {
        self.messages.lock().map_or(0, |messages| messages.len())
    }

    fn fail_uncertain(&self) {
        if let Ok(mut result) = self.result.lock() {
            *result = Err(SmsDeliveryError {
                reason: SmsFailureReason::ProviderNetworkError,
                outcome: histae_api_rust::identity::mobile::sweego::SmsFailureOutcome::Unknown,
            });
        }
    }
}

impl SmsDelivery for FakeDelivery {
    fn send_otp(&self, message: SmsMessage) -> SmsDeliveryFuture<'_> {
        Box::pin(async move {
            self.messages
                .lock()
                .map_err(|_| SmsDeliveryError {
                    reason: SmsFailureReason::DeliveryUnknown,
                    outcome: histae_api_rust::identity::mobile::sweego::SmsFailureOutcome::Unknown,
                })?
                .push(message);
            self.result
                .lock()
                .map_err(|_| SmsDeliveryError {
                    reason: SmsFailureReason::DeliveryUnknown,
                    outcome: histae_api_rust::identity::mobile::sweego::SmsFailureOutcome::Unknown,
                })?
                .clone()
        })
    }
}

#[derive(Clone, Default)]
struct FakeAccounts {
    account: Arc<Mutex<Option<MobileAccount>>>,
    tombstone: Arc<Mutex<bool>>,
}

impl MobileAccountStore for FakeAccounts {
    fn find_by_phone_hash(
        &self,
        _phone_hash: String,
    ) -> AccountStoreFuture<'_, Option<MobileAccount>> {
        Box::pin(async move {
            self.account
                .lock()
                .map(|account| *account)
                .map_err(|_| AccountStoreError::Database(DatabaseError::QueryFailed))
        })
    }

    fn create(&self, account: NewMobileAccount) -> AccountStoreFuture<'_, MobileAccount> {
        Box::pin(async move {
            if self.tombstone.lock().map(|value| *value).unwrap_or(true) {
                return Err(AccountStoreError::Tombstone);
            }
            let created = MobileAccount {
                user_id: account.user_id,
                is_banned: false,
            };
            *self
                .account
                .lock()
                .map_err(|_| AccountStoreError::Database(DatabaseError::QueryFailed))? =
                Some(created);
            Ok(created)
        })
    }
}

#[derive(Clone, Default)]
struct FakeSessions;

impl MobileSessionStore for FakeSessions {
    fn create(
        &self,
        user_id: Uuid,
        token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        Box::pin(async move {
            Ok(Some(MobileSessionIdentity {
                user_id,
                session_id: token.id,
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
        Box::pin(async { Ok(None) })
    }
}

struct Fixture {
    app: Router,
    otp_store: FakeOtpStore,
    delivery: FakeDelivery,
    accounts: FakeAccounts,
    webhook_secret: SecretString,
}

fn fixture(otp_limit: u64) -> Fixture {
    let hash_key = SecretString::new("hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh".to_owned());
    let otp_store = FakeOtpStore::default();
    let delivery = FakeDelivery::default();
    let otp = OtpService::new(
        Arc::new(otp_store.clone()),
        Arc::new(delivery.clone()),
        hash_key.clone(),
        "FR".to_owned(),
        Duration::from_secs(10),
        Duration::from_secs(600),
    );
    let jwt_secret = SecretString::new("jwt-signing-secret-0123456789abcdef".to_owned());
    let tokens = TokenService::new(JwtConfig {
        secret: jwt_secret.clone(),
        active_kid: "primary".to_owned(),
        verification_keys: BTreeMap::from([("primary".to_owned(), jwt_secret)]),
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(3_600),
    });
    let auth = MobileAuthService::new(
        tokens,
        Arc::new(FakeSessions),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let accounts = FakeAccounts::default();
    let login = MobileLoginService::new(
        otp.clone(),
        Arc::new(accounts.clone()),
        auth,
        SecretString::new("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned()),
    );
    let limiter = RateLimiter::memory(&hash_key);
    let otp_policy = LimitPolicy {
        max: otp_limit,
        window: Duration::from_secs(3_600),
    };
    let secret = SecretString::new(STANDARD.encode([0x42_u8; 48]));
    let sms = SmsConfig {
        provider: SmsProvider::Sweego,
        endpoint: "https://api.sweego.test/send".parse().expect("URL"),
        api_key: SecretString::new("fixture-api-key".to_owned()),
        sender_id: "Histae".to_owned(),
        region: "FR".to_owned(),
        timeout: Duration::from_secs(10),
        otp_ttl: Duration::from_secs(600),
        webhook_secret: secret.clone(),
    };
    let webhook = SweegoWebhookService::new(
        &sms,
        Arc::new(otp_store.clone()),
        Arc::new(SweegoWebhookMetrics::default()),
    );
    let routes = otp_routes(OtpHttpState::new(otp, login, limiter.clone(), otp_policy)).merge(
        sweego_routes(SweegoHttpState::new(
            webhook,
            limiter.clone(),
            LimitPolicy {
                max: 300,
                window: Duration::from_secs(60),
            },
        )),
    );
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let state = HttpState::new(
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
    Fixture {
        app: build_router(routes, state),
        otp_store,
        delivery,
        accounts,
        webhook_secret: secret,
    }
}

fn request(uri: &str, body: &str, idempotency_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", key);
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
async fn send_is_strict_normalizes_the_phone_and_replays_without_a_second_sms() {
    let fixture = fixture(5);
    let body = r#"{"phone_number":"+33 6 12 34 56 78"}"#;
    let first = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/send", body, Some(idempotency_key())))
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(
        json_body(first).await,
        json!({ "message": "Verification code request accepted." })
    );
    assert_eq!(fixture.delivery.calls(), 1);
    assert_eq!(fixture.otp_store.attempt_count(), 1);

    let replay = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/send", body, Some(idempotency_key())))
        .await
        .expect("response");
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(fixture.delivery.calls(), 1);

    let noncanonical = idempotency_key().replace('-', "");
    let noncanonical_key = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/send", body, Some(&noncanonical)))
        .await
        .expect("response");
    assert_eq!(noncanonical_key.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(noncanonical_key).await["error"]["code"],
        "invalid_idempotency_key"
    );

    let unknown_field = fixture
        .app
        .oneshot(request(
            "/api/auth/otp/send",
            r#"{"phone_number":"+33612345678","role":"admin"}"#,
            Some(idempotency_key()),
        ))
        .await
        .expect("response");
    assert_eq!(unknown_field.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(unknown_field).await["error"]["code"],
        "invalid_request_body"
    );
}

#[tokio::test]
async fn phone_validation_precedes_quotas_but_idempotency_validation_consumes_them() {
    let fixture = fixture(1);
    let invalid_phone = fixture
        .app
        .clone()
        .oneshot(request(
            "/api/auth/otp/send",
            r#"{"phone_number":"+442071838750"}"#,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(invalid_phone).await["error"]["code"],
        "invalid_phone_number"
    );

    let invalid_key = fixture
        .app
        .clone()
        .oneshot(request(
            "/api/auth/otp/send",
            &format!(r#"{{"phone_number":"{PHONE}"}}"#),
            Some("not-a-uuid"),
        ))
        .await
        .expect("response");
    assert_eq!(invalid_key.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid_key).await["error"]["code"],
        "invalid_idempotency_key"
    );

    let limited = fixture
        .app
        .oneshot(request(
            "/api/auth/otp/send",
            &format!(r#"{{"phone_number":"{PHONE}"}}"#),
            Some(idempotency_key()),
        ))
        .await
        .expect("response");
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        json_body(limited).await["error"]["code"],
        "otp_rate_limit_exceeded"
    );
}

#[tokio::test]
async fn uncertain_delivery_is_never_reposted_and_a_callback_can_recover_the_replay() {
    let fixture = fixture(5);
    fixture.delivery.fail_uncertain();
    let body = &format!(r#"{{"phone_number":"{PHONE}"}}"#);
    let uncertain = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/send", body, Some(idempotency_key())))
        .await
        .expect("response");
    assert_eq!(uncertain.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(uncertain).await["error"]["code"],
        "otp_delivery_unknown"
    );
    assert_eq!(fixture.delivery.calls(), 1);

    fixture.otp_store.attempts.lock().expect("attempts")[0].state = OtpDeliveryState::Sent;
    let recovered = fixture
        .app
        .oneshot(request("/api/auth/otp/send", body, Some(idempotency_key())))
        .await
        .expect("response");
    assert_eq!(recovered.status(), StatusCode::ACCEPTED);
    assert_eq!(fixture.delivery.calls(), 1);
}

#[tokio::test]
async fn verify_creates_an_account_issues_tokens_and_consumes_the_code_once() {
    let fixture = fixture(5);
    let sent = fixture
        .app
        .clone()
        .oneshot(request(
            "/api/auth/otp/send",
            &format!(r#"{{"phone_number":"{PHONE}"}}"#),
            Some(idempotency_key()),
        ))
        .await
        .expect("response");
    assert_eq!(sent.status(), StatusCode::ACCEPTED);
    let code = fixture.delivery.last_code().expect("generated OTP");
    let verify_body = format!(r#"{{"phone_number":"{PHONE}","otp":"{code}"}}"#);
    let verified = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/verify", &verify_body, None))
        .await
        .expect("response");
    assert_eq!(verified.status(), StatusCode::OK);
    let payload = json_body(verified).await;
    assert!(
        payload["access_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        payload["refresh_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(fixture.accounts.account.lock().expect("account").is_some());

    let replay = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/verify", &verify_body, None))
        .await
        .expect("response");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(replay).await["error"]["code"],
        "invalid_or_expired_otp"
    );
}

#[tokio::test]
async fn verify_distinguishes_malformed_and_nonexistent_codes() {
    let fixture = fixture(5);
    let malformed = fixture
        .app
        .clone()
        .oneshot(request(
            "/api/auth/otp/verify",
            &format!(r#"{{"phone_number":"{PHONE}","otp":"12a"}}"#),
            None,
        ))
        .await
        .expect("response");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(malformed).await["error"]["code"],
        "invalid_otp_request"
    );

    let absent = fixture
        .app
        .oneshot(request(
            "/api/auth/otp/verify",
            &format!(r#"{{"phone_number":"{PHONE}","otp":"123456"}}"#),
            None,
        ))
        .await
        .expect("response");
    assert_eq!(absent.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(absent).await["error"]["code"],
        "invalid_or_expired_otp"
    );
}

#[tokio::test]
async fn a_tombstone_rejects_account_creation_after_irreversible_otp_consumption() {
    let fixture = fixture(5);
    *fixture.accounts.tombstone.lock().expect("tombstone") = true;
    fixture
        .app
        .clone()
        .oneshot(request(
            "/api/auth/otp/send",
            &format!(r#"{{"phone_number":"{PHONE}"}}"#),
            Some(idempotency_key()),
        ))
        .await
        .expect("response");
    let code = fixture.delivery.last_code().expect("generated OTP");
    let body = format!(r#"{{"phone_number":"{PHONE}","otp":"{code}"}}"#);
    let blocked = fixture
        .app
        .clone()
        .oneshot(request("/api/auth/otp/verify", &body, None))
        .await
        .expect("response");
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(blocked).await["error"]["code"],
        "account_unavailable"
    );
    let retry = fixture
        .app
        .oneshot(request("/api/auth/otp/verify", &body, None))
        .await
        .expect("response");
    assert_eq!(retry.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_authenticates_the_exact_raw_body_and_maps_provider_outcomes() {
    let fixture = fixture(5);
    let delivery_id = Uuid::new_v4();
    let event = json!({
        "event_type": "sms_sent",
        "channel": "sms",
        "test_mode": false,
        "sender_id": "Histae",
        "timestamp": "2026-09-21T12:00:00Z",
        "event_id": Uuid::new_v4(),
        "campaign_id": delivery_id,
        "swg_uid": "message-1",
        "transaction_id": "transaction-1",
        "phone_number": "+33600000000"
    });
    let body = serde_json::to_vec_pretty(&event).expect("event JSON");
    let headers = sign(&body, &fixture.webhook_secret);
    let valid = webhook_request(&body, &headers);
    let response = fixture.app.clone().oneshot(valid).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, json!({ "received": true }));

    let reserialized = serde_json::to_vec(&event).expect("event JSON");
    let forged = fixture
        .app
        .clone()
        .oneshot(webhook_request(&reserialized, &headers))
        .await
        .expect("response");
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(forged).await["error"]["code"],
        "invalid_sweego_signature"
    );

    *fixture.otp_store.webhook_outcome.lock().expect("outcome") = SmsEventOutcome::Conflict;
    let conflict = fixture
        .app
        .oneshot(webhook_request(&body, &headers))
        .await
        .expect("response");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(conflict).await["error"]["code"],
        "sweego_delivery_conflict"
    );
}

fn sign(body: &[u8], secret: &SecretString) -> [String; 3] {
    let id = "event_fixture".to_owned();
    let timestamp = Utc::now().timestamp().to_string();
    let decoded = STANDARD
        .decode(secret.expose_secret())
        .expect("fixture secret");
    let mut hmac = Hmac::<Sha256>::new_from_slice(&decoded).expect("HMAC key");
    hmac.update(id.as_bytes());
    hmac.update(b".");
    hmac.update(timestamp.as_bytes());
    hmac.update(b".");
    hmac.update(body);
    [id, timestamp, STANDARD.encode(hmac.finalize().into_bytes())]
}

fn webhook_request(body: &[u8], headers: &[String; 3]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/auth/sweego/webhook")
        .header("webhook-id", &headers[0])
        .header("webhook-timestamp", &headers[1])
        .header("webhook-signature", &headers[2])
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_vec()))
        .expect("request")
}
