use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use hmac::{Hmac, Mac};
use reqwest::Method;
use serde_json::Value;
use sha2::Sha256;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::domain::{BillingPeriod, StripeSubscriptionStatus};
use super::stripe::{StripeClient, StripeError};
use crate::config::{BillingConfig, BillingProvider};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::notifications::domain::NotificationIntent;
use crate::notifications::enqueue_notification;
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

#[derive(Clone)]
pub struct PgStripeWebhookStore {
    database: Database,
}

impl PgStripeWebhookStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl StripeWebhookStore for PgStripeWebhookStore {
    fn processed(&self, event_id: &str) -> WebhookStoreFuture<'_, bool> {
        let event_id = event_id.to_owned();
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query_scalar::<_, i32>("SELECT 1 FROM stripe_webhook_event WHERE id = $1")
                .bind(event_id)
                .fetch_optional(&mut *connection)
                .await
                .map(|row| row.is_some())
                .map_err(map_sqlx_error)
                .map_err(WebhookStoreError::from)
        })
    }

    fn apply(&self, command: WebhookCommand) -> WebhookStoreFuture<'_, WebhookProcessResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let metadata = command.metadata();
                        let inserted: Option<String> = sqlx::query_scalar(
                            "INSERT INTO stripe_webhook_event
                               (id, event_type, object_id, livemode, api_version, stripe_created_at)
                             VALUES ($1, $2, $3, $4, $5, $6)
                             ON CONFLICT (id) DO NOTHING RETURNING id",
                        )
                        .bind(&metadata.id)
                        .bind(&metadata.event_type)
                        .bind(&metadata.object_id)
                        .bind(metadata.livemode)
                        .bind(&metadata.api_version)
                        .bind(metadata.created_at)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if inserted.is_none() {
                            return Ok(WebhookProcessResult::Duplicate);
                        }
                        match apply_command(connection, command).await {
                            Ok(effect) => Ok(WebhookProcessResult::Applied(effect)),
                            Err(ApplyError::InactiveAccount) => {
                                Ok(WebhookProcessResult::Applied(None))
                            }
                            Err(ApplyError::InvalidMapping) => {
                                Err(WebhookStoreError::InvalidMapping)
                            }
                            Err(ApplyError::Database(error)) => {
                                Err(WebhookStoreError::Database(error))
                            }
                        }
                    })
                })
                .await
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyError {
    Database(DatabaseError),
    InvalidMapping,
    InactiveAccount,
}

impl From<DatabaseError> for ApplyError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

async fn apply_command(
    connection: &mut PgConnection,
    command: WebhookCommand,
) -> Result<Option<WebhookEffect>, ApplyError> {
    match command {
        WebhookCommand::Receipt(_) => Ok(None),
        WebhookCommand::Subscription {
            metadata,
            projection,
            notify_trial_ending,
        } => {
            let user_id = resolve_user(connection, &projection).await?;
            upsert_subscription(connection, user_id, &projection).await?;
            if notify_trial_ending && let Some(trial_ends_at) = projection.trial_ends_at {
                enqueue_notification(
                    connection,
                    user_id,
                    &metadata.id,
                    &NotificationIntent::SubscriptionTrialEnding {
                        subscription_id: projection.stripe_subscription_id.clone(),
                        trial_ends_at,
                    },
                )
                .await?;
            }
            Ok(Some(WebhookEffect {
                user_id,
                subscription_status: Some(projection.status),
            }))
        }
        WebhookCommand::Invoice {
            metadata,
            subscription,
            invoice,
            notify_payment_failed,
        } => {
            let user_id = resolve_user(connection, &subscription).await?;
            upsert_subscription(connection, user_id, &subscription).await?;
            upsert_invoice(connection, user_id, &invoice).await?;
            if notify_payment_failed {
                enqueue_notification(
                    connection,
                    user_id,
                    &metadata.id,
                    &NotificationIntent::BillingPaymentFailed {
                        invoice_id: invoice.stripe_invoice_id.clone(),
                    },
                )
                .await?;
            }
            Ok(Some(WebhookEffect {
                user_id,
                subscription_status: Some(subscription.status),
            }))
        }
        WebhookCommand::Checkout {
            session_id, status, ..
        } => {
            let user_id: Option<Uuid> = sqlx::query_scalar(
                "UPDATE billing_checkout_session
                 SET status = $2, checkout_url = NULL, updated_at = clock_timestamp()
                 WHERE stripe_session_id = $1 AND status IN ('open', 'creating')
                 RETURNING user_id",
            )
            .bind(session_id)
            .bind(status)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(user_id.map(|user_id| WebhookEffect {
                user_id,
                subscription_status: None,
            }))
        }
        WebhookCommand::CustomerDeleted {
            metadata,
            customer_id,
        } => mark_customer_deleted(connection, &customer_id, metadata.created_at).await,
    }
}

async fn resolve_user(
    connection: &mut PgConnection,
    projection: &SubscriptionProjection,
) -> Result<Uuid, ApplyError> {
    let mapped: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM billing_customer WHERE stripe_customer_id = $1")
            .bind(&projection.stripe_customer_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    let owner = mapped.or(projection.metadata_user_id);
    if let Some(owner) = owner {
        require_active_account(connection, owner).await?;
    }
    if let Some(mapped) = mapped {
        if projection
            .metadata_user_id
            .is_some_and(|metadata| metadata != mapped)
        {
            return Err(ApplyError::InvalidMapping);
        }
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM billing_customer
             WHERE stripe_customer_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(&projection.stripe_customer_id)
        .bind(mapped)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        return locked.ok_or(ApplyError::InvalidMapping);
    }
    let user_id = projection
        .metadata_user_id
        .ok_or(ApplyError::InvalidMapping)?;
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO billing_customer (user_id, stripe_customer_id)
         SELECT user_id, $2 FROM user_account WHERE user_id = $1 AND deleted_at IS NULL
         ON CONFLICT (user_id) DO UPDATE SET updated_at = clock_timestamp()
         WHERE billing_customer.stripe_customer_id = EXCLUDED.stripe_customer_id
         RETURNING user_id",
    )
    .bind(user_id)
    .bind(&projection.stripe_customer_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    inserted.ok_or(ApplyError::InvalidMapping)
}

async fn require_active_account(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> Result<(), ApplyError> {
    let row = sqlx::query("SELECT deleted_at FROM user_account WHERE user_id = $1 FOR SHARE")
        .bind(user_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let Some(row) = row else {
        return Err(ApplyError::InvalidMapping);
    };
    let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").map_err(map_sqlx_error)?;
    if deleted_at.is_some() {
        return Err(ApplyError::InactiveAccount);
    }
    Ok(())
}

async fn upsert_subscription(
    connection: &mut PgConnection,
    user_id: Uuid,
    input: &SubscriptionProjection,
) -> Result<(), ApplyError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO user_subscription (
           user_id, plan, provider, provider_subscription_id, provider_price_id,
           billing_period, status, cancel_at_period_end, current_period_starts_at,
           current_period_ends_at, trial_ends_at, canceled_at, provider_event_created_at,
           projection_version, provider_snapshot_at, updated_at
         ) VALUES ($1, 'premium', 'stripe', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 1, $11, clock_timestamp())
         ON CONFLICT (user_id) DO UPDATE SET
           plan = 'premium', provider = 'stripe', provider_subscription_id = EXCLUDED.provider_subscription_id,
           provider_price_id = EXCLUDED.provider_price_id, billing_period = EXCLUDED.billing_period,
           status = EXCLUDED.status, cancel_at_period_end = EXCLUDED.cancel_at_period_end,
           current_period_starts_at = EXCLUDED.current_period_starts_at,
           current_period_ends_at = EXCLUDED.current_period_ends_at,
           trial_ends_at = EXCLUDED.trial_ends_at, canceled_at = EXCLUDED.canceled_at,
           provider_event_created_at = EXCLUDED.provider_event_created_at,
           provider_snapshot_at = GREATEST(user_subscription.provider_snapshot_at, EXCLUDED.provider_snapshot_at),
           projection_version = user_subscription.projection_version + 1,
           updated_at = clock_timestamp()
         WHERE (
             user_subscription.provider_subscription_id IS NULL
             OR user_subscription.provider_subscription_id = EXCLUDED.provider_subscription_id
             OR user_subscription.status NOT IN ('trialing', 'active', 'past_due')
           )
           AND (
             user_subscription.provider_event_created_at IS NULL
             OR user_subscription.provider_event_created_at < EXCLUDED.provider_event_created_at
             OR (
               user_subscription.provider_event_created_at = EXCLUDED.provider_event_created_at
               AND (
                 user_subscription.status NOT IN ('canceled', 'unpaid', 'incomplete_expired')
                 OR EXCLUDED.status IN ('canceled', 'unpaid', 'incomplete_expired')
               )
             )
           )
           AND (
             user_subscription.provider_snapshot_at IS NULL
             OR user_subscription.provider_snapshot_at <= EXCLUDED.provider_snapshot_at
           )
         RETURNING user_id",
    )
    .bind(user_id)
    .bind(&input.stripe_subscription_id)
    .bind(&input.stripe_price_id)
    .bind(input.billing_period.as_str())
    .bind(status_name(input.status))
    .bind(input.cancel_at_period_end)
    .bind(input.current_period_starts_at)
    .bind(input.current_period_ends_at)
    .bind(input.trial_ends_at)
    .bind(input.canceled_at)
    .bind(input.event_created_at)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if updated.is_none() {
        let current: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT provider_subscription_id, provider_event_created_at
             FROM user_subscription WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        let stale_same_subscription = current.is_some_and(|(subscription_id, event_at)| {
            subscription_id == input.stripe_subscription_id
                && event_at.is_some_and(|event_at| event_at >= input.event_created_at)
        });
        if !stale_same_subscription {
            return Err(ApplyError::InvalidMapping);
        }
    }
    if let Some(trial_starts_at) = input.trial_starts_at {
        sqlx::query(
            "UPDATE billing_customer
             SET trial_used_at = COALESCE(trial_used_at, $2), updated_at = clock_timestamp()
             WHERE user_id = $1 AND stripe_customer_id = $3",
        )
        .bind(user_id)
        .bind(trial_starts_at)
        .bind(&input.stripe_customer_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    Ok(())
}

async fn upsert_invoice(
    connection: &mut PgConnection,
    user_id: Uuid,
    input: &InvoiceProjection,
) -> Result<(), ApplyError> {
    sqlx::query(
        "INSERT INTO billing_invoice (
           stripe_invoice_id, user_id, stripe_customer_id, stripe_subscription_id,
           status, currency, amount_due, amount_paid, amount_remaining,
           period_starts_at, period_ends_at, paid_at, created_at,
           provider_event_created_at, updated_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, clock_timestamp())
         ON CONFLICT (stripe_invoice_id) DO UPDATE SET
           user_id = EXCLUDED.user_id, stripe_customer_id = EXCLUDED.stripe_customer_id,
           stripe_subscription_id = EXCLUDED.stripe_subscription_id, status = EXCLUDED.status,
           currency = EXCLUDED.currency, amount_due = EXCLUDED.amount_due,
           amount_paid = EXCLUDED.amount_paid, amount_remaining = EXCLUDED.amount_remaining,
           period_starts_at = EXCLUDED.period_starts_at, period_ends_at = EXCLUDED.period_ends_at,
           paid_at = EXCLUDED.paid_at, provider_event_created_at = EXCLUDED.provider_event_created_at,
           updated_at = clock_timestamp()
         WHERE billing_invoice.provider_event_created_at IS NULL
           OR billing_invoice.provider_event_created_at < EXCLUDED.provider_event_created_at
           OR (
             billing_invoice.provider_event_created_at = EXCLUDED.provider_event_created_at
             AND (
               billing_invoice.status NOT IN ('paid', 'void', 'uncollectible')
               OR EXCLUDED.status IN ('paid', 'void', 'uncollectible')
             )
           )",
    )
    .bind(&input.stripe_invoice_id)
    .bind(user_id)
    .bind(&input.stripe_customer_id)
    .bind(&input.stripe_subscription_id)
    .bind(&input.status)
    .bind(&input.currency)
    .bind(input.amount_due)
    .bind(input.amount_paid)
    .bind(input.amount_remaining)
    .bind(input.period_starts_at)
    .bind(input.period_ends_at)
    .bind(input.paid_at)
    .bind(input.created_at)
    .bind(input.event_created_at)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn mark_customer_deleted(
    connection: &mut PgConnection,
    customer_id: &str,
    event_created_at: DateTime<Utc>,
) -> Result<Option<WebhookEffect>, ApplyError> {
    let user_id: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM billing_customer WHERE stripe_customer_id = $1")
            .bind(customer_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    require_active_account(connection, user_id).await?;
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM billing_customer
         WHERE stripe_customer_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(customer_id)
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if locked.is_none() {
        return Err(ApplyError::InvalidMapping);
    }
    sqlx::query(
        "UPDATE user_subscription
         SET status = 'canceled', cancel_at_period_end = false,
           canceled_at = COALESCE(canceled_at, $2),
           provider_event_created_at = GREATEST(provider_event_created_at, $2),
           provider_snapshot_at = GREATEST(provider_snapshot_at, $2),
           projection_version = projection_version + 1, updated_at = clock_timestamp()
         WHERE user_id = $1 AND provider = 'stripe'
           AND (provider_snapshot_at IS NULL OR provider_snapshot_at <= $2)",
    )
    .bind(user_id)
    .bind(event_created_at)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE billing_checkout_session
         SET status = 'failed', checkout_url = NULL, updated_at = clock_timestamp()
         WHERE user_id = $1 AND status IN ('creating', 'open')",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE billing_customer
         SET stripe_customer_deleted_at = clock_timestamp(), updated_at = clock_timestamp()
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(Some(WebhookEffect {
        user_id,
        subscription_status: Some(StripeSubscriptionStatus::Canceled),
    }))
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
