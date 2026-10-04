use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use histae_api_rust::billing::domain::{BillingPeriod, CheckoutSessionView, SubscriptionView};
use histae_api_rust::billing::http::{
    BillingHttpFuture, BillingHttpState, BillingUseCases, routes,
};
use histae_api_rust::billing::service::BillingError;
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::store::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckoutCall {
    user_id: Uuid,
    period: BillingPeriod,
    idempotency_key: Option<String>,
}

#[derive(Clone)]
struct FakeBilling {
    checkout_error: Option<BillingError>,
    calls: Arc<Mutex<Vec<CheckoutCall>>>,
}

impl FakeBilling {
    fn successful() -> Self {
        Self {
            checkout_error: None,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl BillingUseCases for FakeBilling {
    fn subscription(&self, _user_id: Uuid) -> BillingHttpFuture<'_, SubscriptionView> {
        Box::pin(async { Ok(SubscriptionView::free(false)) })
    }

    fn create_checkout(
        &self,
        user_id: Uuid,
        billing_period: BillingPeriod,
        idempotency_key: Option<&str>,
    ) -> BillingHttpFuture<'_, CheckoutSessionView> {
        let error = self.checkout_error;
        let key = idempotency_key.map(str::to_owned);
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(CheckoutCall {
                user_id,
                period: billing_period,
                idempotency_key: key,
            });
        }
        Box::pin(async move {
            if let Some(error) = error {
                return Err(error);
            }
            Ok(CheckoutSessionView {
                session_id: "cs_test_contract".to_owned(),
                url: "https://checkout.stripe.test/session".to_owned(),
                expires_at: "2026-09-29T12:30:00.000Z".to_owned(),
            })
        })
    }

    fn create_portal(&self, _user_id: Uuid) -> BillingHttpFuture<'_, String> {
        Box::pin(async { Ok("https://billing.stripe.test/portal".to_owned()) })
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

fn app(service: Arc<dyn BillingUseCases>, billing_max: u64) -> (axum::Router, String) {
    let tokens = TokenService::new(jwt_config());
    let auth = MobileAuthService::new(
        tokens.clone(),
        Arc::new(AuthStore),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "rate-limit-secret-0123456789abcdef".to_owned(),
    ));
    let global_policy = LimitPolicy {
        max: 100,
        window: Duration::from_secs(60),
    };
    let billing_policy = LimitPolicy {
        max: billing_max,
        window: Duration::from_secs(60),
    };
    let auth_state = MobileAuthState::new(auth, limiter.clone(), global_policy.clone());
    let billing_state = BillingHttpState::new(service, limiter.clone(), billing_policy);
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http_state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter,
        global_policy,
    )
    .unwrap_or_else(|_| unreachable!());
    let token = tokens
        .access_token(user_id(), session_id())
        .unwrap_or_else(|_| unreachable!());
    (
        build_router(routes(billing_state, auth_state), http_state),
        token,
    )
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    serde_json::from_slice(&bytes).unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn requires_mobile_authentication() {
    let (app, _) = app(Arc::new(FakeBilling::successful()), 10);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/users/me/subscription")
                .body(Body::empty())
                .unwrap_or_else(|_| unreachable!()),
        )
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn exposes_subscription_checkout_and_portal_contracts() {
    let billing = FakeBilling::successful();
    let calls = Arc::clone(&billing.calls);
    let (app, token) = app(Arc::new(billing), 10);

    let subscription = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/subscription",
            "GET",
            &token,
            None,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(subscription.status(), StatusCode::OK);
    assert_eq!(
        json_response(subscription).await,
        json!({
            "plan":"free","provider":null,"status":null,"access_granted":false,
            "billing_period":null,"cancel_at_period_end":false,
            "current_period_starts_at":null,"current_period_ends_at":null,
            "trial_ends_at":null,"canceled_at":null,"customer_portal_available":false
        })
    );

    let key = Uuid::new_v4().hyphenated().to_string();
    let checkout = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/subscription/checkout",
            "POST",
            &token,
            Some(json!({"billing_period":"annual"})),
            Some(&key),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(checkout.status(), StatusCode::CREATED);
    assert_eq!(
        json_response(checkout).await,
        json!({
            "session_id":"cs_test_contract",
            "url":"https://checkout.stripe.test/session",
            "expires_at":"2026-09-29T12:30:00.000Z"
        })
    );
    {
        let recorded = calls.lock().unwrap_or_else(|_| unreachable!());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].user_id, user_id());
        assert_eq!(recorded[0].period, BillingPeriod::Annual);
        assert_eq!(recorded[0].idempotency_key.as_deref(), Some(key.as_str()));
    }

    let portal = app
        .oneshot(authenticated(
            "/api/users/me/subscription/portal",
            "POST",
            &token,
            None,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(portal.status(), StatusCode::CREATED);
    assert_eq!(
        json_response(portal).await,
        json!({"url":"https://billing.stripe.test/portal"})
    );
}

#[tokio::test]
async fn rejects_unknown_fields_and_maps_missing_resources() {
    let missing = FakeBilling {
        checkout_error: Some(BillingError::AccountNotFound),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, token) = app(Arc::new(missing), 10);
    let key = Uuid::new_v4().hyphenated().to_string();
    let invalid = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/subscription/checkout",
            "POST",
            &token,
            Some(json!({"billing_period":"monthly","price_id":"price_client"})),
            Some(&key),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_response(invalid).await["error"]["code"],
        "invalid_checkout_payload"
    );

    let missing_response = app
        .oneshot(authenticated(
            "/api/users/me/subscription/checkout",
            "POST",
            &token,
            Some(json!({"billing_period":"monthly"})),
            Some(&key),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_response(missing_response).await["error"]["code"],
        "account_not_found"
    );
}

#[tokio::test]
async fn enforces_the_dedicated_billing_rate_limit() {
    let (app, token) = app(Arc::new(FakeBilling::successful()), 1);
    let first = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/subscription/portal",
            "POST",
            &token,
            None,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(first.status(), StatusCode::CREATED);
    let second = app
        .oneshot(authenticated(
            "/api/users/me/subscription/portal",
            "POST",
            &token,
            None,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        json_response(second).await["error"]["code"],
        "billing_rate_limit_exceeded"
    );
}

#[tokio::test]
async fn maps_invalid_idempotency_and_database_failures_without_details() {
    let invalid_key = FakeBilling {
        checkout_error: Some(BillingError::InvalidIdempotencyKey),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (invalid_app, token) = app(Arc::new(invalid_key), 10);
    let invalid = invalid_app
        .oneshot(authenticated(
            "/api/users/me/subscription/checkout",
            "POST",
            &token,
            Some(json!({"billing_period":"monthly"})),
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_response(invalid).await["error"]["code"],
        "invalid_idempotency_key"
    );

    let database_failure = FakeBilling {
        checkout_error: Some(BillingError::Database(DatabaseError::QueryFailed)),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (database_app, token) = app(Arc::new(database_failure), 10);
    let database = database_app
        .oneshot(authenticated(
            "/api/users/me/subscription/checkout",
            "POST",
            &token,
            Some(json!({"billing_period":"monthly"})),
            Some(&Uuid::new_v4().hyphenated().to_string()),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(database.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json_response(database).await,
        json!({"error":{"code":"internal_error","message":"The request could not be completed."}})
    );
}

fn authenticated(
    uri: &str,
    method: &str,
    token: &str,
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", key);
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap_or_else(|_| unreachable!())
}
