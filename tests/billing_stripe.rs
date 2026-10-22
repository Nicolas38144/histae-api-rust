use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::billing::domain::BillingPeriod;
use histae_api_rust::billing::stripe::{
    CheckoutInput, STRIPE_API_VERSION, StripeClient, StripeError, StripeGateway,
};
use histae_api_rust::config::{BillingConfig, BillingProvider, SecretString};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

fn config() -> BillingConfig {
    BillingConfig {
        provider: BillingProvider::Stripe,
        stripe_secret_key: SecretString::new("test-secret".to_owned()),
        stripe_webhook_secret: SecretString::new("test-webhook-secret".to_owned()),
        premium_monthly_price_id: "price_monthly_contract".to_owned(),
        premium_annual_price_id: "price_annual_contract".to_owned(),
        checkout_success_url: Some(
            "https://app.histae.test/billing/success?session_id={CHECKOUT_SESSION_ID}".to_owned(),
        ),
        checkout_cancel_url: Some("https://app.histae.test/billing/cancel".to_owned()),
        portal_return_url: Some("https://app.histae.test/settings/subscription".to_owned()),
        automatic_tax: true,
        allow_promotion_codes: false,
        timeout: Duration::from_secs(2),
        max_network_retries: 0,
        reconciliation_interval: Duration::from_secs(300),
        reconciliation_freshness: Duration::from_secs(3_600),
        reconciliation_batch_size: 25,
    }
}

async fn capture_one(response_body: String) -> (Url, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|_| unreachable!());
    let address = listener.local_addr().unwrap_or_else(|_| unreachable!());
    let endpoint = Url::parse(&format!("http://{address}/v1/")).unwrap_or_else(|_| unreachable!());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap_or_else(|_| unreachable!());
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2_048];
        let header_end = loop {
            let read = socket
                .read(&mut buffer)
                .await
                .unwrap_or_else(|_| unreachable!());
            if read == 0 {
                unreachable!();
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = socket
                .read(&mut buffer)
                .await
                .unwrap_or_else(|_| unreachable!());
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .unwrap_or_else(|_| unreachable!());
        String::from_utf8(request).unwrap_or_else(|_| unreachable!())
    });
    (endpoint, task)
}

fn request_parts(request: &str) -> (&str, BTreeMap<String, String>, BTreeMap<String, String>) {
    let (head, body) = request.split_once("\r\n\r\n").unwrap_or((request, ""));
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let form = url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    (request_line, headers, form)
}

#[tokio::test]
async fn customer_creation_keeps_the_original_attempt_and_stripe_contract() {
    let user_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let response = format!(
        r#"{{"id":"cus_{}","object":"customer"}}"#,
        Uuid::new_v4().simple()
    );
    let (endpoint, captured) = capture_one(response).await;
    let client = StripeClient::with_endpoint(config(), endpoint).unwrap_or_else(|_| unreachable!());
    let customer = client
        .create_customer(user_id, attempt_id, format!("histae-customer-{attempt_id}"))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert!(customer.id.starts_with("cus_"));

    let request = captured.await.unwrap_or_else(|_| unreachable!());
    let (line, headers, form) = request_parts(&request);
    assert_eq!(line, "POST /v1/customers HTTP/1.1");
    assert!(
        headers
            .get("user-agent")
            .expect("user agent")
            .starts_with("histae-api-rust/")
    );
    assert!(!headers.contains_key("x-stripe-client-user-agent"));
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer test-secret")
    );
    assert_eq!(
        headers.get("stripe-version").map(String::as_str),
        Some(STRIPE_API_VERSION)
    );
    assert_eq!(
        headers.get("idempotency-key").map(String::as_str),
        Some(format!("histae-customer-{attempt_id}").as_str())
    );
    assert_eq!(
        form.get("metadata[histae_user_id]"),
        Some(&user_id.to_string())
    );
    assert_eq!(
        form.get("metadata[histae_customer_attempt_id]"),
        Some(&attempt_id.to_string())
    );
}

#[tokio::test]
async fn checkout_uses_server_selected_price_and_urls_without_a_trial() {
    let expires_at = Utc::now() + TimeDelta::minutes(30);
    let response = format!(
        r#"{{"id":"cs_{}","url":"https://checkout.stripe.test/session","expires_at":{}}}"#,
        Uuid::new_v4().simple(),
        expires_at.timestamp()
    );
    let (endpoint, captured) = capture_one(response).await;
    let mut changed = config();
    changed.checkout_success_url = Some("https://changed.histae.test/success".into());
    changed.automatic_tax = false;
    changed.allow_promotion_codes = true;
    let client = StripeClient::with_endpoint(changed, endpoint).unwrap_or_else(|_| unreachable!());
    let user_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let session = client
        .create_checkout_session(CheckoutInput {
            locale: None,
            user_id,
            customer_id: format!("cus_{}", Uuid::new_v4().simple()),
            price_id: "price_annual_contract".to_owned(),
            billing_period: BillingPeriod::Annual,
            trial_days: 0,
            expires_at,
            idempotency_key: format!("histae-checkout-{attempt_id}"),
            success_url: config().checkout_success_url.expect("success URL"),
            cancel_url: config().checkout_cancel_url.expect("cancel URL"),
            automatic_tax: true,
            allow_promotion_codes: false,
        })
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        session.url.as_deref(),
        Some("https://checkout.stripe.test/session")
    );

    let request = captured.await.unwrap_or_else(|_| unreachable!());
    let (line, headers, form) = request_parts(&request);
    assert_eq!(line, "POST /v1/checkout/sessions HTTP/1.1");
    assert_eq!(
        headers.get("idempotency-key").map(String::as_str),
        Some(format!("histae-checkout-{attempt_id}").as_str())
    );
    assert_eq!(
        form.get("line_items[0][price]").map(String::as_str),
        Some("price_annual_contract")
    );
    assert!(!form.contains_key("subscription_data[trial_period_days]"));
    assert_eq!(
        form.get("metadata[histae_billing_period]")
            .map(String::as_str),
        Some("annual")
    );
    assert_eq!(
        form.get("automatic_tax[enabled]").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        form.get("customer_update[address]").map(String::as_str),
        Some("auto")
    );
    assert_eq!(
        form.get("allow_promotion_codes").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        form.get("success_url").map(String::as_str),
        Some("https://app.histae.test/billing/success?session_id={CHECKOUT_SESSION_ID}")
    );
}

#[tokio::test]
async fn checkout_rejects_an_old_trial_request_before_contacting_stripe() {
    let client = StripeClient::with_endpoint(
        config(),
        Url::parse("http://127.0.0.1:9/v1/").expect("local endpoint"),
    )
    .expect("test configuration");
    let result = client
        .create_checkout_session(CheckoutInput {
            locale: None,
            user_id: Uuid::new_v4(),
            customer_id: "cus_Legacy".into(),
            price_id: "price_monthly_contract".into(),
            billing_period: BillingPeriod::Monthly,
            trial_days: 30,
            expires_at: Utc::now() + TimeDelta::minutes(35),
            idempotency_key: format!("histae-checkout-{}", Uuid::new_v4()),
            success_url: config().checkout_success_url.expect("success URL"),
            cancel_url: config().checkout_cancel_url.expect("cancel URL"),
            automatic_tax: false,
            allow_promotion_codes: false,
        })
        .await;
    assert_eq!(result, Err(StripeError::InvalidConfiguration));
}

#[tokio::test]
async fn portal_uses_the_linked_customer_and_configured_return_url() {
    let response = r#"{"url":"https://billing.stripe.test/portal"}"#.to_owned();
    let (endpoint, captured) = capture_one(response).await;
    let client = StripeClient::with_endpoint(config(), endpoint).unwrap_or_else(|_| unreachable!());
    let customer_id = format!("cus_{}", Uuid::new_v4().simple());
    let key = format!("histae-portal-{}", Uuid::new_v4());
    let portal = client
        .create_portal_session(&customer_id, key.clone())
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(portal.url, "https://billing.stripe.test/portal");

    let request = captured.await.unwrap_or_else(|_| unreachable!());
    let (line, headers, form) = request_parts(&request);
    assert_eq!(line, "POST /v1/billing_portal/sessions HTTP/1.1");
    assert_eq!(headers.get("idempotency-key"), Some(&key));
    assert_eq!(form.get("customer"), Some(&customer_id));
    assert_eq!(
        form.get("return_url").map(String::as_str),
        Some("https://app.histae.test/settings/subscription")
    );
}

#[tokio::test]
async fn stripe_requests_update_aggregated_metrics() {
    let (endpoint, captured) =
        capture_one(r#"{"id":"cus_fixture","object":"customer"}"#.into()).await;
    let metrics =
        std::sync::Arc::new(histae_api_rust::operations::metrics::OperationalMetrics::new());
    let client = StripeClient::with_endpoint(config(), endpoint)
        .expect("client")
        .with_metrics(metrics.clone());
    let attempt = Uuid::new_v4();
    client
        .create_customer(Uuid::new_v4(), attempt, format!("customer-{attempt}"))
        .await
        .expect("customer");
    captured.await.expect("server");
    let snapshot = metrics.snapshot();
    let (_, counters) = snapshot
        .dependencies
        .iter()
        .find(|(name, _)| *name == "stripe")
        .expect("metrics");
    assert_eq!((counters.calls, counters.errors), (1, 0));
}

#[tokio::test]
async fn existing_customer_subscriptions_are_checked_before_a_second_purchase() {
    for (status, expected) in [
        ("active", true),
        ("past_due", true),
        ("unpaid", true),
        ("paused", true),
        ("incomplete", true),
        ("trialing", true),
        ("canceled", false),
        ("incomplete_expired", false),
    ] {
        let (endpoint, captured) = capture_one(
            serde_json::json!({
                "has_more":false, "data":[{"customer":"cus_Fixture", "status":status}]
            })
            .to_string(),
        )
        .await;
        let client = StripeClient::with_endpoint(config(), endpoint).expect("client");
        assert_eq!(
            client
                .has_current_subscription("cus_Fixture")
                .await
                .expect("subscription list"),
            expected
        );
        let raw = captured.await.expect("server");
        let (line, _, _) = request_parts(&raw);
        assert!(line.starts_with("GET /v1/subscriptions?"));
        assert!(line.contains("customer=cus_Fixture") && line.contains("status=all"));
    }
}

#[tokio::test]
async fn incomplete_or_misrouted_provider_lists_never_allow_another_subscription() {
    for value in [
        serde_json::json!({"has_more":true,"data":[]}),
        serde_json::json!({"has_more":false,"data":[{"customer":"cus_Other","status":"active"}]}),
        serde_json::json!({"has_more":false,"data":[{"customer":"cus_Fixture","status":"future_status"}]}),
    ] {
        let (endpoint, captured) = capture_one(value.to_string()).await;
        let client = StripeClient::with_endpoint(config(), endpoint).expect("client");
        assert!(
            client
                .has_current_subscription("cus_Fixture")
                .await
                .is_err()
        );
        captured.await.expect("server");
    }
}

#[tokio::test]
async fn localized_stripe_sessions_keep_original_persisted_retry_parameters() {
    use histae_api_rust::localization::Language;
    let expires_at = Utc::now() + TimeDelta::minutes(30);
    let input = CheckoutInput {
        locale: Some(Language::Es),
        user_id: Uuid::new_v4(),
        customer_id: "cus_fixture".into(),
        price_id: "price_fixture".into(),
        billing_period: BillingPeriod::Annual,
        trial_days: 0,
        expires_at,
        idempotency_key: "localization-fixture".into(),
        success_url: "https://www.histae.com/es/billing/success?session_id={CHECKOUT_SESSION_ID}"
            .into(),
        cancel_url: "https://www.histae.com/es/billing/cancel".into(),
        automatic_tax: false,
        allow_promotion_codes: false,
    };
    let (endpoint, captured) = capture_one(format!(
        r#"{{"id":"cs_fixture","url":"https://checkout.stripe.test/fixture","expires_at":{}}}"#,
        expires_at.timestamp()
    ))
    .await;
    StripeClient::with_endpoint(config(), endpoint)
        .expect("client")
        .create_checkout_session(input.clone())
        .await
        .expect("checkout");
    let request = captured.await.expect("server");
    let (_, _, form) = request_parts(&request);
    assert_eq!(form.get("locale").map(String::as_str), Some("es"));
    assert_eq!(form.get("success_url"), Some(&input.success_url));
    let mut legacy = serde_json::to_value(&input).expect("serialize");
    legacy.as_object_mut().expect("object").remove("locale");
    let restored: CheckoutInput = serde_json::from_value(legacy).expect("legacy retry");
    assert_eq!(restored.locale, None);
    assert!(
        !serde_json::to_value(restored)
            .expect("serialize")
            .as_object()
            .expect("object")
            .contains_key("locale")
    );
    let mut config = config();
    config.portal_return_url = Some("https://www.histae.com/billing/return".into());
    let (endpoint, captured) =
        capture_one(r#"{"url":"https://billing.stripe.test/fixture"}"#.into()).await;
    StripeClient::with_endpoint(config, endpoint)
        .expect("client")
        .create_portal_session_in("cus_fixture", "portal-fixture".into(), Language::It)
        .await
        .expect("portal");
    let request = captured.await.expect("server");
    let (_, _, form) = request_parts(&request);
    assert_eq!(form.get("locale").map(String::as_str), Some("it"));
    assert_eq!(
        form.get("return_url").map(String::as_str),
        Some("https://www.histae.com/it/billing/return")
    );
}

#[tokio::test]
async fn checkout_expiration_requires_the_matching_session_and_expired_status() {
    for (body, expected) in [
        (serde_json::json!({"id":"cs_switch", "status":"expired"}), Ok(())),
        (serde_json::json!({"id":"cs_other", "status":"expired"}), Err(StripeError::InvalidResponse)),
        (serde_json::json!({"id":"cs_switch", "status":"complete"}), Err(StripeError::Rejected)),
        (serde_json::json!({"id":"cs_switch", "status":"open"}), Err(StripeError::Rejected)),
        (serde_json::json!({"id":"cs_switch"}), Err(StripeError::InvalidResponse)),
    ] {
        let (endpoint, captured) = capture_one(body.to_string()).await;
        let client = StripeClient::with_endpoint(config(), endpoint).expect("client");
        assert_eq!(client.expire_checkout_session("cs_switch").await, expected);
        let raw = captured.await.expect("server");
        assert!(raw.starts_with("POST /v1/checkout/sessions/cs_switch/expire HTTP/1.1"));
    }
}

#[tokio::test]
async fn repeated_expiration_retrieves_state_and_never_accepts_a_completed_checkout() {
    for (status, expected) in [("expired", Ok(())), ("complete", Err(StripeError::Rejected))] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let endpoint = Url::parse(&format!("http://{}/v1/", listener.local_addr().expect("address"))).expect("endpoint");
        let task = tokio::spawn(async move {
            for (code, body, method) in [
                ("400 Bad Request", "{}".to_owned(), "POST /v1/checkout/sessions/cs_switch/expire HTTP/1.1"),
                ("200 OK", serde_json::json!({"id":"cs_switch", "status":status}).to_string(), "GET /v1/checkout/sessions/cs_switch HTTP/1.1"),
            ] {
                let (mut socket, _) = listener.accept().await.expect("connection");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = socket.read(&mut buffer).await.expect("request");
                    assert!(read > 0);
                    request.extend_from_slice(&buffer[..read]);
                }
                assert!(String::from_utf8_lossy(&request).starts_with(method));
                let response = format!("HTTP/1.1 {code}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.expect("response");
            }
        });
        let client = StripeClient::with_endpoint(config(), endpoint).expect("client");
        assert_eq!(client.expire_checkout_session("cs_switch").await, expected);
        task.await.expect("server");
    }
}
