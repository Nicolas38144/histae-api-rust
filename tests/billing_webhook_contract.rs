use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use histae_api_rust::billing::http::{StripeWebhookHttpState, stripe_webhook_routes};
use histae_api_rust::billing::stripe::StripeError;
use histae_api_rust::billing::webhook::{
    NoopBillingRealtimePublisher, StripeWebhookFuture, StripeWebhookGateway, StripeWebhookService,
    StripeWebhookStore, WebhookCommand, WebhookProcessResult, WebhookStoreFuture,
};
use histae_api_rust::config::{
    BillingConfig, BillingProvider, Environment, LimitPolicy, SecretString, TrustProxy,
};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::shared::clock::Clock;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt as _;
use uuid::Uuid;

#[derive(Clone)]
struct Probe;

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Clone, Default)]
struct FakeStore {
    commands: Arc<Mutex<Vec<WebhookCommand>>>,
}

impl StripeWebhookStore for FakeStore {
    fn processed(&self, _event_id: &str) -> WebhookStoreFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn apply(&self, command: WebhookCommand) -> WebhookStoreFuture<'_, WebhookProcessResult> {
        if let Ok(mut commands) = self.commands.lock() {
            commands.push(command);
        }
        Box::pin(async { Ok(WebhookProcessResult::Applied(None)) })
    }
}

#[derive(Clone)]
struct UnusedStripe;

impl StripeWebhookGateway for UnusedStripe {
    fn retrieve_subscription(&self, _subscription_id: &str) -> StripeWebhookFuture<'_, Value> {
        Box::pin(async { Err(StripeError::Network) })
    }
}

fn config() -> BillingConfig {
    BillingConfig {
        provider: BillingProvider::Stripe,
        stripe_secret_key: SecretString::new("sk_test_contract".to_owned()),
        stripe_webhook_secret: SecretString::new("whsec_contract".to_owned()),
        premium_product_id: "prod_Contract".to_owned(),
        premium_monthly_price_id: "price_Monthly".to_owned(),
        premium_annual_price_id: "price_Annual".to_owned(),
        checkout_success_url: None,
        checkout_cancel_url: None,
        portal_return_url: None,
        automatic_tax: false,
        allow_promotion_codes: false,
        timeout: Duration::from_secs(1),
        max_network_retries: 0,
        reconciliation_interval: Duration::from_secs(60),
        reconciliation_freshness: Duration::from_secs(300),
        reconciliation_batch_size: 10,
    }
}

fn fixture(webhook_max: u64) -> (axum::Router, FakeStore, DateTime<Utc>) {
    let now = DateTime::from_timestamp(1_900_000_000, 0).unwrap_or_else(Utc::now);
    let store = FakeStore::default();
    let service = StripeWebhookService::new(
        Arc::new(store.clone()),
        Arc::new(UnusedStripe),
        Arc::new(NoopBillingRealtimePublisher),
        Arc::new(FixedClock(now)),
        config(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "rate-limit-secret-0123456789abcdef".to_owned(),
    ));
    let global = LimitPolicy {
        max: 100,
        window: Duration::from_secs(60),
    };
    let state = StripeWebhookHttpState::new(
        service,
        limiter.clone(),
        LimitPolicy {
            max: webhook_max,
            window: Duration::from_secs(60),
        },
    );
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter,
        global,
    )
    .unwrap_or_else(|_| unreachable!());
    (build_router(stripe_webhook_routes(state), http), store, now)
}

fn checkout_event(now: DateTime<Utc>, livemode: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": format!("evt_{}", Uuid::new_v4().simple()),
        "type": "checkout.session.completed",
        "livemode": livemode,
        "api_version": "2026-07-29.dahlia",
        "created": now.timestamp(),
        "data": {"object": {"id": format!("cs_test_{}", Uuid::new_v4().simple())}}
    }))
    .unwrap_or_else(|_| unreachable!())
}

fn signature(body: &[u8], now: DateTime<Utc>) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(b"whsec_contract").unwrap_or_else(|_| unreachable!());
    mac.update(format!("{}.", now.timestamp()).as_bytes());
    mac.update(body);
    format!("t={},v1={:x}", now.timestamp(), mac.finalize().into_bytes())
}

fn request(body: Vec<u8>, signature: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/billing/stripe/webhook")
        .header("content-type", "application/json")
        .header("stripe-signature", signature)
        .body(Body::from(body))
        .unwrap_or_else(|_| unreachable!())
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    serde_json::from_slice(&bytes).unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn accepts_the_exact_signed_body_and_returns_the_nest_contract() {
    let (app, store, now) = fixture(10);
    let body = checkout_event(now, false);
    let response = app
        .oneshot(request(body.clone(), &signature(&body, now)))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"received": true}));
    let commands = store.commands.lock().unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        commands.as_slice(),
        [WebhookCommand::Checkout {
            status: "completed",
            ..
        }]
    ));
}

#[tokio::test]
async fn rejects_invalid_signatures_before_any_database_effect() {
    let (app, store, now) = fixture(10);
    let body = checkout_event(now, false);
    let response = app
        .oneshot(request(body, "t=1,v1=invalid"))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"]["code"],
        "invalid_stripe_signature"
    );
    assert!(
        store
            .commands
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .is_empty()
    );
}

#[tokio::test]
async fn rejects_live_events_when_the_configured_key_is_for_test_mode() {
    let (app, store, now) = fixture(10);
    let body = checkout_event(now, true);
    let response = app
        .oneshot(request(body.clone(), &signature(&body, now)))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"]["code"],
        "stripe_mode_mismatch"
    );
    assert!(
        store
            .commands
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .is_empty()
    );
}

#[tokio::test]
async fn applies_the_dedicated_webhook_rate_limit() {
    let (app, _store, now) = fixture(1);
    let first = checkout_event(now, false);
    let first_response = app
        .clone()
        .oneshot(request(first.clone(), &signature(&first, now)))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(first_response.status(), StatusCode::OK);

    let second = checkout_event(now, false);
    let second_response = app
        .oneshot(request(second.clone(), &signature(&second, now)))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(second_response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body_json(second_response).await["error"]["code"],
        "billing_webhook_rate_limit_exceeded"
    );
}
