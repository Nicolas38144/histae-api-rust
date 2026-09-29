use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::billing::domain::BillingPeriod;
use histae_api_rust::billing::stripe::{
    CheckoutInput, STRIPE_API_VERSION, StripeClient, StripeGateway,
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
        premium_product_id: "prod_contract".to_owned(),
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
async fn checkout_uses_only_server_selected_price_trial_and_urls() {
    let expires_at = Utc::now() + TimeDelta::minutes(30);
    let response = format!(
        r#"{{"id":"cs_{}","url":"https://checkout.stripe.test/session","expires_at":{}}}"#,
        Uuid::new_v4().simple(),
        expires_at.timestamp()
    );
    let (endpoint, captured) = capture_one(response).await;
    let client = StripeClient::with_endpoint(config(), endpoint).unwrap_or_else(|_| unreachable!());
    let user_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let session = client
        .create_checkout_session(CheckoutInput {
            user_id,
            customer_id: format!("cus_{}", Uuid::new_v4().simple()),
            price_id: "price_annual_contract".to_owned(),
            billing_period: BillingPeriod::Annual,
            trial_days: 30,
            expires_at,
            idempotency_key: format!("histae-checkout-{attempt_id}"),
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
    assert_eq!(
        form.get("subscription_data[trial_period_days]")
            .map(String::as_str),
        Some("30")
    );
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
        form.get("allow_promotion_codes").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        form.get("success_url").map(String::as_str),
        Some("https://app.histae.test/billing/success?session_id={CHECKOUT_SESSION_ID}")
    );
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
