use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use tokio::time::sleep;
use url::Url;
use uuid::Uuid;

use super::domain::BillingPeriod;
use crate::config::{BillingConfig, BillingProvider};

pub const STRIPE_API_VERSION: &str = "2026-07-29.dahlia";
pub const MAX_STRIPE_RESPONSE_BYTES: usize = 65_536;
const STRIPE_ENDPOINT: &str = "https://api.stripe.com/v1/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutInput {
    pub user_id: Uuid,
    pub customer_id: String,
    pub price_id: String,
    pub billing_period: BillingPeriod,
    pub trial_days: i16,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripeCheckoutSession {
    pub id: String,
    pub url: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripeCustomer {
    pub id: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripePortalSession {
    pub url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StripeError {
    NotConfigured,
    InvalidConfiguration,
    Network,
    Rejected,
    InvalidResponse,
}

impl fmt::Display for StripeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotConfigured => "stripe_not_configured",
            Self::InvalidConfiguration => "stripe_invalid_configuration",
            Self::Network => "stripe_network_error",
            Self::Rejected => "stripe_rejected_request",
            Self::InvalidResponse => "stripe_invalid_response",
        })
    }
}

impl std::error::Error for StripeError {}

pub type StripeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StripeError>> + Send + 'a>>;

pub trait StripeGateway: Send + Sync {
    fn create_customer(
        &self,
        user_id: Uuid,
        attempt_id: Uuid,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer>;

    fn create_checkout_session(
        &self,
        input: CheckoutInput,
    ) -> StripeFuture<'_, StripeCheckoutSession>;

    fn expire_checkout_session(&self, session_id: &str) -> StripeFuture<'_, ()>;

    fn create_portal_session(
        &self,
        customer_id: &str,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripePortalSession>;

    fn delete_customer(
        &self,
        customer_id: &str,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer>;

    fn retrieve_customer(&self, customer_id: &str) -> StripeFuture<'_, StripeCustomer>;
}

#[derive(Clone)]
pub struct StripeClient {
    config: BillingConfig,
    endpoint: Url,
    client: reqwest::Client,
}

impl StripeClient {
    pub fn new(config: BillingConfig) -> Result<Self, StripeError> {
        let endpoint =
            Url::parse(STRIPE_ENDPOINT).map_err(|_| StripeError::InvalidConfiguration)?;
        Self::with_endpoint(config, endpoint)
    }

    pub fn with_endpoint(config: BillingConfig, endpoint: Url) -> Result<Self, StripeError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_| StripeError::InvalidConfiguration)?;
        Ok(Self {
            config,
            endpoint,
            client,
        })
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        form: Vec<(String, String)>,
        idempotency_key: Option<&str>,
    ) -> Result<Value, StripeError> {
        if self.config.provider != BillingProvider::Stripe {
            return Err(StripeError::NotConfigured);
        }
        let mut url = self
            .endpoint
            .join(path)
            .map_err(|_| StripeError::InvalidConfiguration)?;
        let body = encode_form(&form);
        if method == Method::GET && !body.is_empty() {
            url.set_query(Some(&body));
        }
        let authorization = format!("Bearer {}", self.config.stripe_secret_key.expose_secret());
        let attempts = usize::from(self.config.max_network_retries) + 1;

        for attempt in 0..attempts {
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .header(AUTHORIZATION, &authorization)
                .header("Stripe-Version", STRIPE_API_VERSION)
                .header(USER_AGENT, "Stripe/v1 NodeBindings/22.4.0 histae-api/3.0.0")
                .header(
                    "X-Stripe-Client-User-Agent",
                    r#"{"bindings_version":"22.4.0","lang":"node","publisher":"stripe"}"#,
                );
            if method != Method::GET {
                request = request
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(body.clone());
            }
            if let Some(key) = idempotency_key {
                request = request.header("Idempotency-Key", key);
            }
            let response = request.send().await;
            match response {
                Ok(response) => {
                    if retryable_status(response.status(), response.headers())
                        && attempt + 1 < attempts
                    {
                        sleep(retry_delay(attempt)).await;
                        continue;
                    }
                    if !response.status().is_success() {
                        return Err(StripeError::Rejected);
                    }
                    return bounded_json(response).await;
                }
                Err(_) if attempt + 1 < attempts => {
                    sleep(retry_delay(attempt)).await;
                }
                Err(_) => return Err(StripeError::Network),
            }
        }
        Err(StripeError::Network)
    }
}

impl StripeGateway for StripeClient {
    fn create_customer(
        &self,
        user_id: Uuid,
        attempt_id: Uuid,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer> {
        Box::pin(async move {
            let value = self
                .request(
                    Method::POST,
                    "customers",
                    vec![
                        (
                            "description".to_owned(),
                            "Histae mobile subscriber".to_owned(),
                        ),
                        (
                            "metadata[histae_user_id]".to_owned(),
                            user_id.hyphenated().to_string(),
                        ),
                        (
                            "metadata[histae_customer_attempt_id]".to_owned(),
                            attempt_id.hyphenated().to_string(),
                        ),
                    ],
                    Some(&idempotency_key),
                )
                .await?;
            parse_customer(value, false)
        })
    }

    fn create_checkout_session(
        &self,
        input: CheckoutInput,
    ) -> StripeFuture<'_, StripeCheckoutSession> {
        Box::pin(async move {
            let success_url = self
                .config
                .checkout_success_url
                .clone()
                .ok_or(StripeError::InvalidConfiguration)?;
            let cancel_url = self
                .config
                .checkout_cancel_url
                .clone()
                .ok_or(StripeError::InvalidConfiguration)?;
            let user_id = input.user_id.hyphenated().to_string();
            let period = input.billing_period.as_str().to_owned();
            let mut form = vec![
                ("mode".to_owned(), "subscription".to_owned()),
                ("origin_context".to_owned(), "mobile_app".to_owned()),
                ("customer".to_owned(), input.customer_id),
                ("client_reference_id".to_owned(), user_id.clone()),
                ("line_items[0][price]".to_owned(), input.price_id),
                ("line_items[0][quantity]".to_owned(), "1".to_owned()),
                ("metadata[histae_user_id]".to_owned(), user_id.clone()),
                ("metadata[histae_plan]".to_owned(), "premium".to_owned()),
                ("metadata[histae_billing_period]".to_owned(), period.clone()),
                (
                    "subscription_data[metadata][histae_user_id]".to_owned(),
                    user_id,
                ),
                (
                    "subscription_data[metadata][histae_plan]".to_owned(),
                    "premium".to_owned(),
                ),
                (
                    "subscription_data[metadata][histae_billing_period]".to_owned(),
                    period,
                ),
                ("payment_method_collection".to_owned(), "always".to_owned()),
                (
                    "allow_promotion_codes".to_owned(),
                    self.config.allow_promotion_codes.to_string(),
                ),
                (
                    "automatic_tax[enabled]".to_owned(),
                    self.config.automatic_tax.to_string(),
                ),
                (
                    "billing_address_collection".to_owned(),
                    if self.config.automatic_tax {
                        "required"
                    } else {
                        "auto"
                    }
                    .to_owned(),
                ),
                ("success_url".to_owned(), success_url),
                ("cancel_url".to_owned(), cancel_url),
                (
                    "expires_at".to_owned(),
                    input.expires_at.timestamp().to_string(),
                ),
            ];
            if input.trial_days > 0 {
                form.push((
                    "subscription_data[trial_period_days]".to_owned(),
                    input.trial_days.to_string(),
                ));
            }
            let value = self
                .request(
                    Method::POST,
                    "checkout/sessions",
                    form,
                    Some(&input.idempotency_key),
                )
                .await?;
            parse_checkout(value)
        })
    }

    fn expire_checkout_session(&self, session_id: &str) -> StripeFuture<'_, ()> {
        let path = format!("checkout/sessions/{session_id}/expire");
        Box::pin(async move {
            self.request(Method::POST, &path, Vec::new(), None)
                .await
                .map(|_| ())
        })
    }

    fn create_portal_session(
        &self,
        customer_id: &str,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripePortalSession> {
        let customer_id = customer_id.to_owned();
        Box::pin(async move {
            let return_url = self
                .config
                .portal_return_url
                .clone()
                .ok_or(StripeError::InvalidConfiguration)?;
            let value = self
                .request(
                    Method::POST,
                    "billing_portal/sessions",
                    vec![
                        ("customer".to_owned(), customer_id),
                        ("return_url".to_owned(), return_url),
                    ],
                    Some(&idempotency_key),
                )
                .await?;
            let url = value
                .get("url")
                .and_then(Value::as_str)
                .filter(|value| valid_hosted_url(value))
                .ok_or(StripeError::InvalidResponse)?;
            Ok(StripePortalSession {
                url: url.to_owned(),
            })
        })
    }

    fn delete_customer(
        &self,
        customer_id: &str,
        idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer> {
        let path = format!("customers/{customer_id}");
        Box::pin(async move {
            let value = self
                .request(Method::DELETE, &path, Vec::new(), Some(&idempotency_key))
                .await?;
            parse_customer(value, true)
        })
    }

    fn retrieve_customer(&self, customer_id: &str) -> StripeFuture<'_, StripeCustomer> {
        let path = format!("customers/{customer_id}");
        Box::pin(async move {
            let value = self.request(Method::GET, &path, Vec::new(), None).await?;
            parse_customer(value, false)
        })
    }
}

fn encode_form(values: &[(String, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in values {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn retryable_status(status: StatusCode, headers: &HeaderMap<HeaderValue>) -> bool {
    match headers
        .get("Stripe-Should-Retry")
        .and_then(|value| value.to_str().ok())
    {
        Some("true") => true,
        Some("false") => false,
        _ => {
            status == StatusCode::CONFLICT
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
        }
    }
}

fn retry_delay(attempt: usize) -> Duration {
    let multiplier = 1_u64 << attempt.min(3);
    Duration::from_millis((500 * multiplier).min(5_000))
}

async fn bounded_json(response: reqwest::Response) -> Result<Value, StripeError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StripeError::InvalidResponse)?;
        let size = body
            .len()
            .checked_add(chunk.len())
            .ok_or(StripeError::InvalidResponse)?;
        if size > MAX_STRIPE_RESPONSE_BYTES {
            return Err(StripeError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| StripeError::InvalidResponse)
}

fn parse_customer(value: Value, require_deleted: bool) -> Result<StripeCustomer, StripeError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| provider_id(value, "cus_"))
        .ok_or(StripeError::InvalidResponse)?;
    let deleted = value
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if require_deleted && !deleted {
        return Err(StripeError::InvalidResponse);
    }
    Ok(StripeCustomer {
        id: id.to_owned(),
        deleted,
    })
}

fn parse_checkout(value: Value) -> Result<StripeCheckoutSession, StripeError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| provider_id(value, "cs_"))
        .ok_or(StripeError::InvalidResponse)?;
    let url = match value.get("url") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if valid_hosted_url(value) => Some(value.clone()),
        _ => return Err(StripeError::InvalidResponse),
    };
    let expires_at = value
        .get("expires_at")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .ok_or(StripeError::InvalidResponse)?;
    Ok(StripeCheckoutSession {
        id: id.to_owned(),
        url,
        expires_at,
    })
}

fn provider_id(value: &str, prefix: &str) -> bool {
    value.len() <= 255
        && value.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

fn valid_hosted_url(value: &str) -> bool {
    value.len() <= 4_096
        && Url::parse(value)
            .ok()
            .is_some_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_preserves_nested_stripe_fields() {
        let body = encode_form(&[
            ("line_items[0][quantity]".to_owned(), "1".to_owned()),
            ("metadata[histae_plan]".to_owned(), "premium".to_owned()),
        ]);
        assert_eq!(
            body,
            "line_items%5B0%5D%5Bquantity%5D=1&metadata%5Bhistae_plan%5D=premium"
        );
    }

    #[test]
    fn validates_provider_identifiers_and_hosted_urls() {
        assert!(provider_id("cus_Histae123", "cus_"));
        assert!(!provider_id("customer_Histae123", "cus_"));
        assert!(valid_hosted_url("https://checkout.stripe.test/session"));
        assert!(!valid_hosted_url("javascript:alert(1)"));
    }
}
