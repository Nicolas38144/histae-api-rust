use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use histae_api_rust::billing::domain::SubscriptionView;
use histae_api_rust::billing::http::{
    BillingHttpFuture, BillingHttpState, BillingUseCases, routes,
};
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
                preferred_language: histae_api_rust::localization::Language::Fr,
                user_id: user_id(),
                role: AccountRole::User,
                is_banned: false,
                onboarding_complete: true,
            }))
        })
    }
}

#[derive(Clone)]
struct FakeBilling;
impl FakeBilling {
    fn successful() -> Self {
        Self
    }
}
impl BillingUseCases for FakeBilling {
    fn subscription(&self, _user_id: Uuid) -> BillingHttpFuture<'_, SubscriptionView> {
        Box::pin(async { Ok(SubscriptionView::free(false)) })
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

fn app(service: Arc<dyn BillingUseCases>) -> (axum::Router, String) {
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
    let auth_state = MobileAuthState::new(auth, limiter.clone(), global_policy.clone());
    let billing_state = BillingHttpState::new(service);
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
    let (app, _) = app(Arc::new(FakeBilling::successful()));
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
async fn exposes_effective_subscription_but_retires_mobile_stripe_mutations() {
    let billing = FakeBilling::successful();
    let (app, token) = app(Arc::new(billing));
    let response = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/subscription",
            "GET",
            &token,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["plan"], "free");
    assert_eq!(body["access_granted"], false);
    assert!(body["management_url"].is_null());
    for path in [
        "/api/users/me/subscription/checkout",
        "/api/users/me/subscription/portal",
    ] {
        let response = app
            .clone()
            .oneshot(authenticated(
                path,
                "POST",
                &token,
                Some(json!({"billing_period":"annual"})),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            json_response(response).await["error"]["code"],
            "billing_web_only"
        );
    }
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
