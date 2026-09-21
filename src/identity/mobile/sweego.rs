use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt as _;
use hmac::{Hmac, Mac as _};
use reqwest::redirect::Policy;
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use uuid::{Uuid, Variant};

use crate::config::{SecretString, SmsConfig, SmsProvider};

use super::otp::{OtpStore, SmsDeliveryEvent, SmsEventKind, SmsEventOutcome};

pub const MAX_SMS_PROVIDER_BODY_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsFailureReason {
    NotConfigured,
    ProviderRejected,
    ProviderUnavailable,
    ProviderNetworkError,
    ProviderInvalidResponse,
    DeliveryUnknown,
    ProviderUndelivered,
}

impl SmsFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::ProviderRejected => "provider_rejected",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderNetworkError => "provider_network_error",
            Self::ProviderInvalidResponse => "provider_invalid_response",
            Self::DeliveryUnknown => "delivery_unknown",
            Self::ProviderUndelivered => "provider_undelivered",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsFailureOutcome {
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmsDeliveryError {
    pub reason: SmsFailureReason,
    pub outcome: SmsFailureOutcome,
}

impl SmsDeliveryError {
    const fn failed(reason: SmsFailureReason) -> Self {
        Self {
            reason,
            outcome: SmsFailureOutcome::Failed,
        }
    }

    const fn unknown(reason: SmsFailureReason) -> Self {
        Self {
            reason,
            outcome: SmsFailureOutcome::Unknown,
        }
    }
}

impl fmt::Display for SmsDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason.as_str())
    }
}

impl std::error::Error for SmsDeliveryError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmsMessage {
    pub phone: String,
    pub region: String,
    pub code: String,
    pub delivery_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmsDeliveryReceipt {
    pub transaction_id: String,
    pub message_id: String,
}

pub type SmsDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SmsDeliveryReceipt, SmsDeliveryError>> + Send + 'a>>;

pub trait SmsDelivery: Send + Sync {
    fn send_otp(&self, message: SmsMessage) -> SmsDeliveryFuture<'_>;
}

#[derive(Clone)]
pub struct SweegoSmsService {
    config: SmsConfig,
    client: reqwest::Client,
}

impl SweegoSmsService {
    pub fn new(config: SmsConfig) -> Result<Self, SmsDeliveryError> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_| SmsDeliveryError::unknown(SmsFailureReason::ProviderNetworkError))?;
        Ok(Self { config, client })
    }

    async fn deliver(&self, message: SmsMessage) -> Result<SmsDeliveryReceipt, SmsDeliveryError> {
        if self.config.provider != SmsProvider::Sweego {
            return Err(SmsDeliveryError::failed(SmsFailureReason::NotConfigured));
        }
        let minutes = self.config.otp_ttl.as_secs().div_ceil(60);
        let payload = SweegoRequest {
            channel: "sms",
            provider: "sweego",
            campaign_type: "transac",
            campaign_id: message.delivery_id.hyphenated().to_string(),
            sender_id: &self.config.sender_id,
            recipients: [SweegoRecipient {
                number: &message.phone,
                region: &message.region,
            }],
            text: format!(
                "Histae : votre code de verification est {}. Il expire dans {minutes} minutes.",
                message.code
            ),
            shorten_urls: false,
            shorten_with_protocol: false,
        };
        let response = self
            .client
            .post(self.config.endpoint.clone())
            .header("Api-Key", self.config.api_key.expose_secret())
            .json(&payload)
            .send()
            .await
            .map_err(|_| SmsDeliveryError::unknown(SmsFailureReason::ProviderNetworkError))?;
        if response.status() != reqwest::StatusCode::OK {
            let rejected = matches!(
                response.status().as_u16(),
                400 | 401 | 403 | 404 | 405 | 413 | 415 | 422 | 429
            );
            return Err(if rejected {
                SmsDeliveryError::failed(SmsFailureReason::ProviderRejected)
            } else {
                SmsDeliveryError::unknown(SmsFailureReason::ProviderUnavailable)
            });
        }
        let value = bounded_json(response).await?;
        parse_receipt(value)
    }
}

impl SmsDelivery for SweegoSmsService {
    fn send_otp(&self, message: SmsMessage) -> SmsDeliveryFuture<'_> {
        Box::pin(self.deliver(message))
    }
}

#[derive(Serialize)]
struct SweegoRequest<'a> {
    channel: &'static str,
    provider: &'static str,
    #[serde(rename = "campaign-type")]
    campaign_type: &'static str,
    #[serde(rename = "campaign-id")]
    campaign_id: String,
    #[serde(rename = "sender-id")]
    sender_id: &'a str,
    recipients: [SweegoRecipient<'a>; 1],
    #[serde(rename = "message-txt")]
    text: String,
    #[serde(rename = "shorten-urls")]
    shorten_urls: bool,
    #[serde(rename = "shorten-with-protocol")]
    shorten_with_protocol: bool,
}

#[derive(Serialize)]
struct SweegoRecipient<'a> {
    #[serde(rename = "num")]
    number: &'a str,
    region: &'a str,
}

async fn bounded_json(response: reqwest::Response) -> Result<Value, SmsDeliveryError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| SmsDeliveryError::unknown(SmsFailureReason::ProviderInvalidResponse))?;
        let size = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| SmsDeliveryError::unknown(SmsFailureReason::ProviderInvalidResponse))?;
        if size > MAX_SMS_PROVIDER_BODY_BYTES {
            return Err(SmsDeliveryError::unknown(
                SmsFailureReason::ProviderInvalidResponse,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| SmsDeliveryError::unknown(SmsFailureReason::ProviderInvalidResponse))
}

fn parse_receipt(value: Value) -> Result<SmsDeliveryReceipt, SmsDeliveryError> {
    let invalid = || SmsDeliveryError::unknown(SmsFailureReason::ProviderInvalidResponse);
    let Value::Object(mut object) = value else {
        return Err(invalid());
    };
    let transaction_id = object
        .remove("transaction_id")
        .and_then(|value| value.as_str().map(str::to_owned))
        .filter(|value| provider_identifier(value))
        .ok_or_else(invalid)?;
    let Value::Object(message_ids) = object.remove("swg_uids").ok_or_else(invalid)? else {
        return Err(invalid());
    };
    if message_ids.len() != 1 {
        return Err(invalid());
    }
    let message_id = message_ids
        .into_values()
        .next()
        .and_then(|value| value.as_str().map(str::to_owned))
        .filter(|value| provider_identifier(value))
        .ok_or_else(invalid)?;
    Ok(SmsDeliveryReceipt {
        transaction_id,
        message_id,
    })
}

pub fn provider_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SweegoWebhookHeaders {
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub signature: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweegoWebhookError {
    Disabled,
    Unavailable,
    InvalidSignature,
    InvalidEvent,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebhookMetric {
    Applied,
    Ignored,
    Conflict,
    InvalidSignature,
    InvalidEvent,
    Unavailable,
    Disabled,
}

#[derive(Debug, Default)]
pub struct SweegoWebhookMetrics {
    applied: AtomicU64,
    ignored: AtomicU64,
    conflict: AtomicU64,
    invalid_signature: AtomicU64,
    invalid_event: AtomicU64,
    unavailable: AtomicU64,
    disabled: AtomicU64,
}

impl SweegoWebhookMetrics {
    pub fn record(&self, outcome: WebhookMetric) {
        let counter = match outcome {
            WebhookMetric::Applied => &self.applied,
            WebhookMetric::Ignored => &self.ignored,
            WebhookMetric::Conflict => &self.conflict,
            WebhookMetric::InvalidSignature => &self.invalid_signature,
            WebhookMetric::InvalidEvent => &self.invalid_event,
            WebhookMetric::Unavailable => &self.unavailable,
            WebhookMetric::Disabled => &self.disabled,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
        BTreeMap::from([
            ("applied", self.applied.load(Ordering::Relaxed)),
            ("ignored", self.ignored.load(Ordering::Relaxed)),
            ("conflict", self.conflict.load(Ordering::Relaxed)),
            (
                "invalid_signature",
                self.invalid_signature.load(Ordering::Relaxed),
            ),
            ("invalid_event", self.invalid_event.load(Ordering::Relaxed)),
            ("unavailable", self.unavailable.load(Ordering::Relaxed)),
            ("disabled", self.disabled.load(Ordering::Relaxed)),
        ])
    }
}

#[derive(Clone)]
pub struct SweegoWebhookService {
    provider: SmsProvider,
    sender_id: Arc<str>,
    secret: SecretString,
    store: Arc<dyn OtpStore>,
    metrics: Arc<SweegoWebhookMetrics>,
}

impl SweegoWebhookService {
    pub fn new(
        config: &SmsConfig,
        store: Arc<dyn OtpStore>,
        metrics: Arc<SweegoWebhookMetrics>,
    ) -> Self {
        Self {
            provider: config.provider,
            sender_id: config.sender_id.clone().into(),
            secret: config.webhook_secret.clone(),
            store,
            metrics,
        }
    }

    pub async fn handle(
        &self,
        body: &[u8],
        headers: &SweegoWebhookHeaders,
    ) -> Result<(), SweegoWebhookError> {
        if self.provider != SmsProvider::Sweego || self.secret.expose_secret().is_empty() {
            self.metrics.record(WebhookMetric::Disabled);
            return Err(SweegoWebhookError::Disabled);
        }
        if verify_signature(body, headers, &self.secret, UtcMillis::now()).is_err() {
            self.metrics.record(WebhookMetric::InvalidSignature);
            return Err(SweegoWebhookError::InvalidSignature);
        }
        let value = serde_json::from_slice(body).map_err(|_| {
            self.metrics.record(WebhookMetric::InvalidEvent);
            SweegoWebhookError::InvalidEvent
        })?;
        let event = parse_event(value, &self.sender_id).map_err(|_| {
            self.metrics.record(WebhookMetric::InvalidEvent);
            SweegoWebhookError::InvalidEvent
        })?;
        let Some(event) = event else {
            self.metrics.record(WebhookMetric::Ignored);
            return Ok(());
        };
        let outcome = self.store.apply_sms_event(event).await.map_err(|_| {
            self.metrics.record(WebhookMetric::Unavailable);
            SweegoWebhookError::Unavailable
        })?;
        match outcome {
            SmsEventOutcome::Applied => self.metrics.record(WebhookMetric::Applied),
            SmsEventOutcome::Ignored => self.metrics.record(WebhookMetric::Ignored),
            SmsEventOutcome::Conflict => {
                self.metrics.record(WebhookMetric::Conflict);
                return Err(SweegoWebhookError::Conflict);
            }
        }
        Ok(())
    }

    pub fn metrics(&self) -> &SweegoWebhookMetrics {
        &self.metrics
    }
}

struct UtcMillis;

impl UtcMillis {
    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

pub fn verify_signature(
    body: &[u8],
    headers: &SweegoWebhookHeaders,
    secret: &SecretString,
    now_millis: i64,
) -> Result<(), SweegoWebhookError> {
    let id = headers
        .id
        .as_deref()
        .filter(|value| provider_identifier(value))
        .ok_or(SweegoWebhookError::InvalidSignature)?;
    let timestamp = headers
        .timestamp
        .as_deref()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 12
                && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or(SweegoWebhookError::InvalidSignature)?;
    let signature = headers
        .signature
        .as_deref()
        .filter(|value| {
            value.len() == 44
                && value.ends_with('=')
                && value[..43]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        })
        .ok_or(SweegoWebhookError::InvalidSignature)?;
    let secret_value = secret.expose_secret();
    if body.len() > MAX_SMS_PROVIDER_BODY_BYTES
        || secret_value.len() != 64
        || !secret_value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
    {
        return Err(SweegoWebhookError::InvalidSignature);
    }
    let timestamp_seconds = timestamp
        .parse::<i64>()
        .map_err(|_| SweegoWebhookError::InvalidSignature)?;
    let signed_at = timestamp_seconds
        .checked_mul(1_000)
        .ok_or(SweegoWebhookError::InvalidSignature)?;
    let age = now_millis
        .checked_sub(signed_at)
        .ok_or(SweegoWebhookError::InvalidSignature)?;
    if !(-60_000..=300_000).contains(&age) {
        return Err(SweegoWebhookError::InvalidSignature);
    }
    let supplied = STANDARD
        .decode(signature)
        .map_err(|_| SweegoWebhookError::InvalidSignature)?;
    if STANDARD.encode(&supplied) != signature {
        return Err(SweegoWebhookError::InvalidSignature);
    }
    let decoded_secret = STANDARD
        .decode(secret_value)
        .map_err(|_| SweegoWebhookError::InvalidSignature)?;
    let mut hmac = Hmac::<Sha256>::new_from_slice(&decoded_secret)
        .map_err(|_| SweegoWebhookError::InvalidSignature)?;
    hmac.update(id.as_bytes());
    hmac.update(b".");
    hmac.update(timestamp.as_bytes());
    hmac.update(b".");
    hmac.update(body);
    let expected = hmac.finalize().into_bytes();
    if supplied.as_slice().ct_eq(expected.as_slice()).into() {
        Ok(())
    } else {
        Err(SweegoWebhookError::InvalidSignature)
    }
}

pub fn parse_event(
    value: Value,
    sender_id: &str,
) -> Result<Option<SmsDeliveryEvent>, SweegoWebhookError> {
    let Value::Object(object) = value else {
        return Err(SweegoWebhookError::InvalidEvent);
    };
    let event_type = object
        .get("event_type")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
        .ok_or(SweegoWebhookError::InvalidEvent)?;
    let kind = match event_type {
        "sms_sent" => SmsEventKind::Sent,
        "sms_undelivered" => SmsEventKind::Undelivered,
        _ => return Ok(None),
    };
    for (key, field) in &object {
        if matches!(key.as_str(), "event_type" | "test_mode") {
            continue;
        }
        if STRING_FIELDS.contains(&key.as_str())
            && field
                .as_str()
                .is_some_and(|value| value.encode_utf16().count() <= 512)
        {
            continue;
        }
        if NUMBER_FIELDS.contains(&key.as_str()) && field.as_f64().is_some_and(|value| value >= 0.0)
        {
            continue;
        }
        return Err(SweegoWebhookError::InvalidEvent);
    }
    if object.get("channel").and_then(Value::as_str) != Some("sms")
        || object.get("test_mode").and_then(Value::as_bool).is_none()
        || object.get("sender_id").and_then(Value::as_str).is_none()
        || object.get("timestamp").and_then(Value::as_str).is_none()
        || !object
            .get("event_id")
            .and_then(Value::as_str)
            .is_some_and(valid_uuid_all)
        || object.get("campaign_id").and_then(Value::as_str).is_none()
        || !object
            .get("swg_uid")
            .and_then(Value::as_str)
            .is_some_and(provider_identifier)
        || object
            .get("transaction_id")
            .is_some_and(|value| !value.as_str().is_some_and(provider_identifier))
    {
        return Err(SweegoWebhookError::InvalidEvent);
    }
    let test_mode = object
        .get("test_mode")
        .and_then(Value::as_bool)
        .ok_or(SweegoWebhookError::InvalidEvent)?;
    let sender = object
        .get("sender_id")
        .and_then(Value::as_str)
        .ok_or(SweegoWebhookError::InvalidEvent)?;
    let campaign = object
        .get("campaign_id")
        .and_then(Value::as_str)
        .ok_or(SweegoWebhookError::InvalidEvent)?;
    let delivery_id = Uuid::parse_str(campaign).ok().filter(|id| {
        canonical_uuid(campaign, *id)
            && id.get_version_num() == 4
            && id.get_variant() == Variant::RFC4122
    });
    if test_mode || sender != sender_id || delivery_id.is_none() {
        return Ok(None);
    }
    Ok(Some(SmsDeliveryEvent {
        delivery_id: delivery_id.ok_or(SweegoWebhookError::InvalidEvent)?,
        message_id: object
            .get("swg_uid")
            .and_then(Value::as_str)
            .ok_or(SweegoWebhookError::InvalidEvent)?
            .to_owned(),
        transaction_id: object
            .get("transaction_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        kind,
    }))
}

fn valid_uuid_all(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| {
        canonical_uuid(value, id)
            && (1..=8).contains(&id.get_version_num())
            && id.get_variant() == Variant::RFC4122
    })
}

fn canonical_uuid(value: &str, id: Uuid) -> bool {
    value.len() == 36 && id.hyphenated().to_string().eq_ignore_ascii_case(value)
}

const STRING_FIELDS: &[&str] = &[
    "timestamp",
    "swg_uid",
    "event_id",
    "details",
    "channel",
    "client-id",
    "client_id",
    "country_code",
    "phone_number",
    "sender_id",
    "sms_type",
    "campaign_id",
    "transaction_id",
    "send_date",
    "status",
];

const NUMBER_FIELDS: &[&str] = &[
    "sms_price",
    "nb_segments",
    "mobile_network_code",
    "mobile_country_code",
];
