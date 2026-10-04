mod pg;
pub use pg::PgStripeWebhookStore;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use hmac::{Hmac, Mac};
use reqwest::Method;
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

use crate::billing::domain::{BillingPeriod, StripeSubscriptionStatus};
use crate::billing::stripe::{StripeClient, StripeError};
use crate::config::{BillingConfig, BillingProvider};
use crate::infra::postgres::DatabaseError;
use crate::shared::clock::Clock;

const SIGNATURE_TOLERANCE_SECONDS: i64 = 300;

const SUBSCRIPTION_EVENTS: &[&str] = &[
    "customer.subscription.created",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "customer.subscription.paused",
    "customer.subscription.resumed",
    "customer.subscription.trial_will_end",
];
const INVOICE_EVENTS: &[&str] = &[
    "invoice.paid",
    "invoice.payment_failed",
    "invoice.payment_action_required",
    "invoice.finalization_failed",
];
const CHECKOUT_EVENTS: &[&str] = &["checkout.session.completed", "checkout.session.expired"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionProjection {
    pub metadata_user_id: Option<Uuid>,
    pub stripe_customer_id: String,
    pub stripe_subscription_id: String,
    pub stripe_price_id: String,
    pub billing_period: BillingPeriod,
    pub status: StripeSubscriptionStatus,
    pub cancel_at_period_end: bool,
    pub current_period_starts_at: DateTime<Utc>,
    pub current_period_ends_at: DateTime<Utc>,
    pub trial_starts_at: Option<DateTime<Utc>>,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
    pub event_created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvoiceProjection {
    pub stripe_invoice_id: String,
    pub stripe_customer_id: String,
    pub stripe_subscription_id: Option<String>,
    pub status: Option<String>,
    pub currency: String,
    pub amount_due: i64,
    pub amount_paid: i64,
    pub amount_remaining: i64,
    pub period_starts_at: DateTime<Utc>,
    pub period_ends_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub event_created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookMetadata {
    pub id: String,
    pub event_type: String,
    pub object_id: Option<String>,
    pub livemode: bool,
    pub api_version: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebhookCommand {
    Receipt(WebhookMetadata),
    Subscription {
        metadata: WebhookMetadata,
        projection: SubscriptionProjection,
        notify_trial_ending: bool,
    },
    Invoice {
        metadata: WebhookMetadata,
        subscription: SubscriptionProjection,
        invoice: Box<InvoiceProjection>,
        notify_payment_failed: bool,
    },
    Checkout {
        metadata: WebhookMetadata,
        session_id: String,
        status: &'static str,
    },
    CustomerDeleted {
        metadata: WebhookMetadata,
        customer_id: String,
    },
}

impl WebhookCommand {
    fn metadata(&self) -> &WebhookMetadata {
        match self {
            Self::Receipt(metadata)
            | Self::Subscription { metadata, .. }
            | Self::Invoice { metadata, .. }
            | Self::Checkout { metadata, .. }
            | Self::CustomerDeleted { metadata, .. } => metadata,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookEffect {
    pub user_id: Uuid,
    pub subscription_status: Option<StripeSubscriptionStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebhookProcessResult {
    Duplicate,
    Applied(Option<WebhookEffect>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebhookStoreError {
    Database(DatabaseError),
    InvalidMapping,
}

impl From<DatabaseError> for WebhookStoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl fmt::Display for WebhookStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Database(error) => error.safe_code(),
            Self::InvalidMapping => "invalid_stripe_event",
        })
    }
}

impl std::error::Error for WebhookStoreError {}

pub type WebhookStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, WebhookStoreError>> + Send + 'a>>;

pub trait StripeWebhookStore: Send + Sync {
    fn processed(&self, event_id: &str) -> WebhookStoreFuture<'_, bool>;
    fn apply(&self, command: WebhookCommand) -> WebhookStoreFuture<'_, WebhookProcessResult>;
}

pub type StripeWebhookFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, StripeError>> + Send + 'a>>;

pub trait StripeWebhookGateway: Send + Sync {
    fn retrieve_subscription(&self, subscription_id: &str) -> StripeWebhookFuture<'_, Value>;
}

impl StripeWebhookGateway for StripeClient {
    fn retrieve_subscription(&self, subscription_id: &str) -> StripeWebhookFuture<'_, Value> {
        let path = format!("subscriptions/{subscription_id}");
        Box::pin(async move { self.request(Method::GET, &path, Vec::new(), None).await })
    }
}

pub type RealtimeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>>;

pub trait BillingRealtimePublisher: Send + Sync {
    fn subscription_updated(
        &self,
        user_id: Uuid,
        status: StripeSubscriptionStatus,
    ) -> RealtimeFuture<'_>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopBillingRealtimePublisher;

impl BillingRealtimePublisher for NoopBillingRealtimePublisher {
    fn subscription_updated(
        &self,
        _user_id: Uuid,
        _status: StripeSubscriptionStatus,
    ) -> RealtimeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StripeWebhookError {
    BillingUnavailable,
    InvalidSignature,
    ModeMismatch,
    InvalidEvent,
    StripeRequestFailed,
    Database(DatabaseError),
}

impl fmt::Display for StripeWebhookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BillingUnavailable => "billing_unavailable",
            Self::InvalidSignature => "invalid_stripe_signature",
            Self::ModeMismatch => "stripe_mode_mismatch",
            Self::InvalidEvent => "invalid_stripe_event",
            Self::StripeRequestFailed => "stripe_request_failed",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for StripeWebhookError {}

#[derive(Clone)]
pub struct StripeWebhookService {
    store: Arc<dyn StripeWebhookStore>,
    stripe: Arc<dyn StripeWebhookGateway>,
    realtime: Arc<dyn BillingRealtimePublisher>,
    clock: Arc<dyn Clock>,
    config: Arc<BillingConfig>,
}

impl StripeWebhookService {
    pub fn new(
        store: Arc<dyn StripeWebhookStore>,
        stripe: Arc<dyn StripeWebhookGateway>,
        realtime: Arc<dyn BillingRealtimePublisher>,
        clock: Arc<dyn Clock>,
        config: BillingConfig,
    ) -> Self {
        Self {
            store,
            stripe,
            realtime,
            clock,
            config: Arc::new(config),
        }
    }

    pub async fn handle(
        &self,
        raw_body: &[u8],
        signature: Option<&str>,
    ) -> Result<(), StripeWebhookError> {
        if self.config.provider != BillingProvider::Stripe {
            return Err(StripeWebhookError::BillingUnavailable);
        }
        let signature = signature.ok_or(StripeWebhookError::InvalidSignature)?;
        verify_signature(
            raw_body,
            signature,
            self.config.stripe_webhook_secret.expose_secret(),
            self.clock.now(),
        )?;
        let event = parse_event(raw_body)?;
        let live_key = self
            .config
            .stripe_secret_key
            .expose_secret()
            .starts_with("sk_live_");
        if event.metadata.livemode != live_key {
            return Err(StripeWebhookError::ModeMismatch);
        }
        if !supported_event(&event.metadata.event_type) {
            return Ok(());
        }
        if self
            .store
            .processed(&event.metadata.id)
            .await
            .map_err(map_store_error)?
        {
            return Ok(());
        }

        let command = self.command(event).await?;
        let result = self.store.apply(command).await.map_err(map_store_error)?;
        if let WebhookProcessResult::Applied(Some(effect)) = result
            && let Some(status) = effect.subscription_status
        {
            let _ = self
                .realtime
                .subscription_updated(effect.user_id, status)
                .await;
        }
        Ok(())
    }

    async fn command(&self, event: ParsedEvent) -> Result<WebhookCommand, StripeWebhookError> {
        let event_type = event.metadata.event_type.clone();
        if SUBSCRIPTION_EVENTS.contains(&event_type.as_str()) {
            let projection =
                map_subscription(&event.object, event.metadata.created_at, &self.config)?;
            return Ok(WebhookCommand::Subscription {
                metadata: event.metadata,
                projection,
                notify_trial_ending: event_type == "customer.subscription.trial_will_end",
            });
        }
        if INVOICE_EVENTS.contains(&event_type.as_str()) {
            let subscription_id = invoice_subscription_id(&event.object)?;
            let Some(subscription_id) = subscription_id else {
                return Ok(WebhookCommand::Receipt(event.metadata));
            };
            let raw_subscription = self
                .stripe
                .retrieve_subscription(&subscription_id)
                .await
                .map_err(|_| StripeWebhookError::StripeRequestFailed)?;
            let subscription =
                map_subscription(&raw_subscription, event.metadata.created_at, &self.config)?;
            let invoice = map_invoice(&event.object, event.metadata.created_at)?;
            if subscription.stripe_customer_id != invoice.stripe_customer_id {
                return Err(StripeWebhookError::InvalidEvent);
            }
            return Ok(WebhookCommand::Invoice {
                metadata: event.metadata,
                subscription,
                invoice: Box::new(invoice),
                notify_payment_failed: matches!(
                    event_type.as_str(),
                    "invoice.payment_failed" | "invoice.payment_action_required"
                ),
            });
        }
        if CHECKOUT_EVENTS.contains(&event_type.as_str()) {
            let session_id = required_provider_id(&event.object, "id", "cs_")?.to_owned();
            return Ok(WebhookCommand::Checkout {
                metadata: event.metadata,
                session_id,
                status: if event_type == "checkout.session.completed" {
                    "completed"
                } else {
                    "expired"
                },
            });
        }
        let customer_id = required_provider_id(&event.object, "id", "cus_")?.to_owned();
        Ok(WebhookCommand::CustomerDeleted {
            metadata: event.metadata,
            customer_id,
        })
    }
}

struct ParsedEvent {
    metadata: WebhookMetadata,
    object: Value,
}

fn parse_event(raw_body: &[u8]) -> Result<ParsedEvent, StripeWebhookError> {
    let value: Value =
        serde_json::from_slice(raw_body).map_err(|_| StripeWebhookError::InvalidEvent)?;
    let id = required_provider_id(&value, "id", "evt_")?.to_owned();
    let event_type = required_string(&value, "type")?;
    if event_type.len() > 150 {
        return Err(StripeWebhookError::InvalidEvent);
    }
    let livemode = value
        .get("livemode")
        .and_then(Value::as_bool)
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let created_at = unix_date(required_i64(&value, "created")?)?;
    let object = value
        .pointer("/data/object")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let object_id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .map(str::to_owned);
    let api_version = match value.get("api_version") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.len() <= 100 => Some(value.clone()),
        _ => return Err(StripeWebhookError::InvalidEvent),
    };
    Ok(ParsedEvent {
        metadata: WebhookMetadata {
            id,
            event_type,
            object_id,
            livemode,
            api_version,
            created_at,
        },
        object,
    })
}

fn supported_event(event_type: &str) -> bool {
    SUBSCRIPTION_EVENTS.contains(&event_type)
        || INVOICE_EVENTS.contains(&event_type)
        || CHECKOUT_EVENTS.contains(&event_type)
        || event_type == "customer.deleted"
}

fn verify_signature(
    body: &[u8],
    header: &str,
    secret: &str,
    now: DateTime<Utc>,
) -> Result<(), StripeWebhookError> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        match name {
            "t" if timestamp.is_none() => timestamp = value.parse::<i64>().ok(),
            "v1" => {
                if let Some(decoded) = decode_hex(value) {
                    signatures.push(decoded);
                }
            }
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(StripeWebhookError::InvalidSignature)?;
    if (now.timestamp() - timestamp).abs() > SIGNATURE_TOLERANCE_SECONDS {
        return Err(StripeWebhookError::InvalidSignature);
    }
    let mut signed = timestamp.to_string().into_bytes();
    signed.push(b'.');
    signed.extend_from_slice(body);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| StripeWebhookError::InvalidSignature)?;
    mac.update(&signed);
    if signatures
        .iter()
        .any(|candidate| mac.clone().verify_slice(candidate).is_ok())
    {
        Ok(())
    } else {
        Err(StripeWebhookError::InvalidSignature)
    }
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 || !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

pub fn map_subscription(
    value: &Value,
    event_created_at: DateTime<Utc>,
    config: &BillingConfig,
) -> Result<SubscriptionProjection, StripeWebhookError> {
    let stripe_subscription_id = required_provider_id(value, "id", "sub_")?.to_owned();
    let stripe_customer_id = object_id(
        value
            .get("customer")
            .ok_or(StripeWebhookError::InvalidEvent)?,
        "cus_",
    )?;
    let status = StripeSubscriptionStatus::parse(required_string_ref(value, "status")?)
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let items = value
        .pointer("/items/data")
        .and_then(Value::as_array)
        .filter(|items| items.len() == 1)
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let item = &items[0];
    let price = item
        .get("price")
        .filter(|value| value.is_object())
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let product = object_id(
        price
            .get("product")
            .ok_or(StripeWebhookError::InvalidEvent)?,
        "prod_",
    )?;
    if product != config.premium_product_id {
        return Err(StripeWebhookError::InvalidEvent);
    }
    let stripe_price_id = required_provider_id(price, "id", "price_")?.to_owned();
    let billing_period = if stripe_price_id == config.premium_monthly_price_id {
        BillingPeriod::Monthly
    } else if stripe_price_id == config.premium_annual_price_id {
        BillingPeriod::Annual
    } else {
        return Err(StripeWebhookError::InvalidEvent);
    };
    let metadata_user_id = value
        .pointer("/metadata/histae_user_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    Ok(SubscriptionProjection {
        metadata_user_id,
        stripe_customer_id,
        stripe_subscription_id,
        stripe_price_id,
        billing_period,
        status,
        cancel_at_period_end: value
            .get("cancel_at_period_end")
            .and_then(Value::as_bool)
            .ok_or(StripeWebhookError::InvalidEvent)?,
        current_period_starts_at: unix_date(required_i64(item, "current_period_start")?)?,
        current_period_ends_at: unix_date(required_i64(item, "current_period_end")?)?,
        trial_starts_at: optional_unix_date(value.get("trial_start"))?,
        trial_ends_at: optional_unix_date(value.get("trial_end"))?,
        canceled_at: optional_unix_date(value.get("canceled_at"))?,
        event_created_at,
    })
}

fn map_invoice(
    value: &Value,
    event_created_at: DateTime<Utc>,
) -> Result<InvoiceProjection, StripeWebhookError> {
    let status = match value.get("status") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.len() <= 64 => Some(value.clone()),
        _ => return Err(StripeWebhookError::InvalidEvent),
    };
    let currency = required_string_ref(value, "currency")?.to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(StripeWebhookError::InvalidEvent);
    }
    Ok(InvoiceProjection {
        stripe_invoice_id: required_provider_id(value, "id", "in_")?.to_owned(),
        stripe_customer_id: object_id(
            value
                .get("customer")
                .ok_or(StripeWebhookError::InvalidEvent)?,
            "cus_",
        )?,
        stripe_subscription_id: invoice_subscription_id(value)?,
        status,
        currency,
        amount_due: required_i64(value, "amount_due")?,
        amount_paid: required_i64(value, "amount_paid")?,
        amount_remaining: required_i64(value, "amount_remaining")?,
        period_starts_at: unix_date(required_i64(value, "period_start")?)?,
        period_ends_at: unix_date(required_i64(value, "period_end")?)?,
        paid_at: optional_unix_date(value.pointer("/status_transitions/paid_at"))?,
        created_at: unix_date(required_i64(value, "created")?)?,
        event_created_at,
    })
}

fn invoice_subscription_id(value: &Value) -> Result<Option<String>, StripeWebhookError> {
    match value.pointer("/parent/subscription_details/subscription") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => object_id(value, "sub_").map(Some),
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, StripeWebhookError> {
    required_string_ref(value, field).map(str::to_owned)
}

fn required_string_ref<'a>(value: &'a Value, field: &str) -> Result<&'a str, StripeWebhookError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(StripeWebhookError::InvalidEvent)
}

fn required_provider_id<'a>(
    value: &'a Value,
    field: &str,
    prefix: &str,
) -> Result<&'a str, StripeWebhookError> {
    required_string_ref(value, field)?
        .strip_prefix(prefix)
        .filter(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .map(|_| required_string_ref(value, field))
        .transpose()?
        .ok_or(StripeWebhookError::InvalidEvent)
}

fn object_id(value: &Value, prefix: &str) -> Result<String, StripeWebhookError> {
    let id = if let Some(value) = value.as_str() {
        value
    } else {
        required_string_ref(value, "id")?
    };
    let suffix = id
        .strip_prefix(prefix)
        .filter(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .ok_or(StripeWebhookError::InvalidEvent)?;
    let _ = suffix;
    Ok(id.to_owned())
}

fn required_i64(value: &Value, field: &str) -> Result<i64, StripeWebhookError> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or(StripeWebhookError::InvalidEvent)
}

fn unix_date(seconds: i64) -> Result<DateTime<Utc>, StripeWebhookError> {
    DateTime::from_timestamp(seconds, 0).ok_or(StripeWebhookError::InvalidEvent)
}

fn optional_unix_date(value: Option<&Value>) -> Result<Option<DateTime<Utc>>, StripeWebhookError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .ok_or(StripeWebhookError::InvalidEvent)
            .and_then(unix_date)
            .map(Some),
    }
}

fn map_store_error(error: WebhookStoreError) -> StripeWebhookError {
    match error {
        WebhookStoreError::Database(error) => StripeWebhookError::Database(error),
        WebhookStoreError::InvalidMapping => StripeWebhookError::InvalidEvent,
    }
}

pub const fn status_name(status: StripeSubscriptionStatus) -> &'static str {
    match status {
        StripeSubscriptionStatus::Incomplete => "incomplete",
        StripeSubscriptionStatus::IncompleteExpired => "incomplete_expired",
        StripeSubscriptionStatus::Trialing => "trialing",
        StripeSubscriptionStatus::Active => "active",
        StripeSubscriptionStatus::PastDue => "past_due",
        StripeSubscriptionStatus::Canceled => "canceled",
        StripeSubscriptionStatus::Unpaid => "unpaid",
        StripeSubscriptionStatus::Paused => "paused",
    }
}

pub fn reconciliation_due(
    snapshot_at: DateTime<Utc>,
    freshness: std::time::Duration,
) -> DateTime<Utc> {
    TimeDelta::from_std(freshness)
        .ok()
        .and_then(|delta| snapshot_at.checked_add_signed(delta))
        .unwrap_or(snapshot_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretString;

    #[test]
    fn verifies_the_exact_raw_body_and_rejects_old_signatures() {
        let body = br#"{"id":"evt_Test","type":"ignored","livemode":false,"created":1,"data":{"object":{}}}"#;
        let now = DateTime::from_timestamp(2_000, 0).unwrap_or_else(Utc::now);
        let timestamp = now.timestamp();
        let mut mac =
            Hmac::<Sha256>::new_from_slice(b"whsec_test").unwrap_or_else(|_| unreachable!());
        mac.update(format!("{timestamp}.").as_bytes());
        mac.update(body);
        let signature = format!("t={timestamp},v1={:x}", mac.finalize().into_bytes());
        assert_eq!(
            verify_signature(body, &signature, "whsec_test", now),
            Ok(())
        );
        assert_eq!(
            verify_signature(
                body,
                &signature,
                "whsec_test",
                now + TimeDelta::seconds(301)
            ),
            Err(StripeWebhookError::InvalidSignature)
        );
        assert_eq!(
            verify_signature(b"{}", &signature, "whsec_test", now),
            Err(StripeWebhookError::InvalidSignature)
        );
    }

    #[test]
    fn maps_only_the_configured_single_premium_price() {
        let config = BillingConfig {
            provider: BillingProvider::Stripe,
            stripe_secret_key: SecretString::new("sk_test_value".to_owned()),
            stripe_webhook_secret: SecretString::new("whsec_value".to_owned()),
            premium_product_id: "prod_Premium".to_owned(),
            premium_monthly_price_id: "price_Monthly".to_owned(),
            premium_annual_price_id: "price_Annual".to_owned(),
            checkout_success_url: None,
            checkout_cancel_url: None,
            portal_return_url: None,
            automatic_tax: false,
            allow_promotion_codes: false,
            timeout: std::time::Duration::from_secs(1),
            max_network_retries: 0,
            reconciliation_interval: std::time::Duration::from_secs(60),
            reconciliation_freshness: std::time::Duration::from_secs(300),
            reconciliation_batch_size: 10,
        };
        let user_id = Uuid::new_v4();
        let value = serde_json::json!({
            "id":"sub_Test", "customer":"cus_Test", "status":"active",
            "cancel_at_period_end":false,
            "metadata":{"histae_user_id":user_id},
            "items":{"data":[{"price":{"id":"price_Monthly","product":"prod_Premium"},
                "current_period_start":1_900_000_000_i64,"current_period_end":1_900_086_400_i64}]},
            "trial_start":null,"trial_end":null,"canceled_at":null
        });
        let projection =
            map_subscription(&value, Utc::now(), &config).unwrap_or_else(|_| unreachable!());
        assert_eq!(projection.metadata_user_id, Some(user_id));
        assert_eq!(projection.billing_period, BillingPeriod::Monthly);

        let mut invalid = value;
        invalid["items"]["data"][0]["price"]["product"] = Value::String("prod_Other".to_owned());
        assert_eq!(
            map_subscription(&invalid, Utc::now(), &config),
            Err(StripeWebhookError::InvalidEvent)
        );
    }
}
