use super::{domain::*, store::OtpStore};
use crate::localization::Language;
use crate::{
    config::SecretString,
    identity::mobile::sweego::{SmsDelivery, SmsMessage},
    infra::crypto::hmac_sha256,
};
use std::{fmt, sync::Arc, time::Duration};
use uuid::{Builder, Uuid};
const DELIVERY_SETTLEMENT_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtpError {
    InvalidPhone,
    InvalidOtpRequest,
    InvalidOtp,
    InvalidIdempotencyKey,
    IdempotencyConflict,
    DeliveryUnavailable,
    DeliveryUnknown,
    PhoneRateLimited,
    Internal,
}

impl fmt::Display for OtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPhone => "invalid_phone_number",
            Self::InvalidOtpRequest => "invalid_otp_request",
            Self::InvalidOtp => "invalid_or_expired_otp",
            Self::InvalidIdempotencyKey => "invalid_idempotency_key",
            Self::IdempotencyConflict => "idempotency_key_conflict",
            Self::DeliveryUnavailable => "otp_delivery_unavailable",
            Self::DeliveryUnknown => "otp_delivery_unknown",
            Self::PhoneRateLimited => "otp_rate_limit_exceeded",
            Self::Internal => "internal_error",
        })
    }
}

impl std::error::Error for OtpError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumedOtp {
    pub phone: String,
    pub phone_hash: String,
}

#[derive(Clone)]
pub struct OtpService {
    store: Arc<dyn OtpStore>,
    delivery: Arc<dyn SmsDelivery>,
    hash_key: SecretString,
    region: Arc<str>,
    provider_timeout: Duration,
    otp_ttl: Duration,
    hash_context: Option<Uuid>,
}

impl OtpService {
    pub fn new(
        store: Arc<dyn OtpStore>,
        delivery: Arc<dyn SmsDelivery>,
        hash_key: SecretString,
        region: String,
        provider_timeout: Duration,
        otp_ttl: Duration,
    ) -> Self {
        Self {
            store,
            delivery,
            hash_key,
            region: region.into(),
            provider_timeout,
            otp_ttl,
            hash_context: None,
        }
    }

    pub fn rate_limit_key(&self, phone_input: &str, invalid: OtpError) -> Result<String, OtpError> {
        let phone = normalize_phone(phone_input).ok_or(invalid)?;
        hmac_sha256(&phone, &self.hash_key).map_err(|_| OtpError::Internal)
    }

    pub fn parse_idempotency_key(input: Option<&str>) -> Result<Uuid, OtpError> {
        normalize_idempotency_key(input)
    }

    pub fn canonical_phone(input: &str) -> Result<String, OtpError> {
        normalize_phone(input).ok_or(OtpError::InvalidPhone)
    }

    /// A website code is bound to its opaque challenge and cannot authenticate
    /// a mobile login or a different website challenge.
    pub fn for_web_challenge(&self, challenge: Uuid) -> Self {
        let mut scoped = self.clone();
        scoped.hash_context = Some(challenge);
        scoped
    }

    fn code_hash(&self, code: &str) -> Result<String, OtpError> {
        let input = self
            .hash_context
            .map_or_else(|| code.to_owned(), |id| format!("web-billing:{id}:{code}"));
        hmac_sha256(&input, &self.hash_key).map_err(|_| OtpError::Internal)
    }

    pub async fn send(
        &self,
        phone_input: &str,
        idempotency_input: Option<&str>,
    ) -> Result<(), OtpError> {
        self.send_in(phone_input, idempotency_input, Language::Fr)
            .await
    }

    pub async fn send_in(
        &self,
        phone_input: &str,
        idempotency_input: Option<&str>,
        language: Language,
    ) -> Result<(), OtpError> {
        let phone = normalize_phone(phone_input).ok_or(OtpError::InvalidPhone)?;
        let idempotency_key = normalize_idempotency_key(idempotency_input)?;
        let code = generate_otp()?;
        let phone_hash = hmac_sha256(&phone, &self.hash_key).map_err(|_| OtpError::Internal)?;
        let otp_hash = self.code_hash(&code)?;
        let delivery_id = random_uuid_v4()?;
        let settlement = self
            .provider_timeout
            .checked_add(DELIVERY_SETTLEMENT_GRACE)
            .ok_or(OtpError::Internal)?;
        let start = self
            .store
            .begin(BeginOtpDelivery {
                language,
                id: delivery_id,
                phone_hash: phone_hash.clone(),
                otp_hash,
                idempotency_key,
                ttl: self.otp_ttl,
                settlement,
            })
            .await
            .map_err(|_| OtpError::Internal)?;
        match start {
            OtpDeliveryStart::Conflict => return Err(OtpError::IdempotencyConflict),
            OtpDeliveryStart::PolicyRejected(reason) => return Err(policy_error(reason)),
            OtpDeliveryStart::Existing(
                OtpDeliveryState::Pending | OtpDeliveryState::Accepted | OtpDeliveryState::Sent,
                _,
            ) => return Ok(()),
            OtpDeliveryStart::Existing(OtpDeliveryState::Unknown, _) => {
                return Err(OtpError::DeliveryUnknown);
            }
            OtpDeliveryStart::Existing(OtpDeliveryState::Failed, _) => {
                return Err(OtpError::DeliveryUnavailable);
            }
            OtpDeliveryStart::Created(_) => {}
        }
        let receipt = match self
            .delivery
            .send_otp(SmsMessage {
                language,
                phone,
                region: self.region.to_string(),
                code,
                delivery_id,
            })
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let state = if error.outcome
                    == crate::identity::mobile::sweego::SmsFailureOutcome::Failed
                {
                    OtpDeliveryState::Failed
                } else {
                    OtpDeliveryState::Unknown
                };
                let persisted = self
                    .store
                    .mark_outcome(delivery_id, phone_hash, state, error.reason)
                    .await
                    .map_err(|_| OtpError::DeliveryUnknown)?;
                if persisted == OtpDeliveryState::Failed && error.reason.is_budget_limit() {
                    return Err(OtpError::PhoneRateLimited);
                }
                return match persisted {
                    OtpDeliveryState::Accepted | OtpDeliveryState::Sent => Ok(()),
                    OtpDeliveryState::Unknown => Err(OtpError::DeliveryUnknown),
                    _ => Err(OtpError::DeliveryUnavailable),
                };
            }
        };
        let confirmed = self
            .store
            .mark_accepted(
                delivery_id,
                phone_hash,
                receipt.transaction_id,
                receipt.message_id,
            )
            .await
            .map_err(|_| OtpError::DeliveryUnknown)?;
        if !confirmed {
            return Err(OtpError::DeliveryUnavailable);
        }
        Ok(())
    }

    pub async fn consume(&self, phone_input: &str, otp: &str) -> Result<ConsumedOtp, OtpError> {
        let phone = normalize_phone(phone_input).ok_or(OtpError::InvalidOtpRequest)?;
        if otp.len() != 6 || !otp.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(OtpError::InvalidOtpRequest);
        }
        let phone_hash = hmac_sha256(&phone, &self.hash_key).map_err(|_| OtpError::Internal)?;
        let otp_hash = self.code_hash(otp)?;
        if !self
            .store
            .consume(phone_hash.clone(), otp_hash)
            .await
            .map_err(|_| OtpError::Internal)?
        {
            return Err(OtpError::InvalidOtp);
        }
        Ok(ConsumedOtp { phone, phone_hash })
    }
}

fn policy_error(reason: crate::identity::mobile::sweego::SmsFailureReason) -> OtpError {
    if reason.is_budget_limit() {
        OtpError::PhoneRateLimited
    } else {
        OtpError::DeliveryUnavailable
    }
}

fn normalize_phone(input: &str) -> Option<String> {
    let normalized = input
        .trim()
        .chars()
        .filter(|character| !matches!(character, ' ' | '.' | '(' | ')' | '-'))
        .collect::<String>();
    let bytes = normalized.as_bytes();
    if bytes.len() == 12
        && bytes.starts_with(b"+33")
        && matches!(bytes[3], b'1'..=b'9')
        && bytes[4..].iter().all(u8::is_ascii_digit)
    {
        Some(normalized)
    } else {
        None
    }
}

fn normalize_idempotency_key(input: Option<&str>) -> Result<Uuid, OtpError> {
    let value = input.unwrap_or_default().trim().to_ascii_lowercase();
    let parsed = Uuid::parse_str(&value).map_err(|_| OtpError::InvalidIdempotencyKey)?;
    if parsed.hyphenated().to_string() != value
        || parsed.get_version_num() != 4
        || parsed.get_variant() != uuid::Variant::RFC4122
    {
        return Err(OtpError::InvalidIdempotencyKey);
    }
    Ok(parsed)
}

fn generate_otp() -> Result<String, OtpError> {
    const RANGE: u32 = 1_000_000;
    const ACCEPT_BELOW: u32 = u32::MAX - (u32::MAX % RANGE);
    loop {
        let mut bytes = [0_u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| OtpError::Internal)?;
        let value = u32::from_le_bytes(bytes);
        if value < ACCEPT_BELOW {
            return Ok(format!("{:06}", value % RANGE));
        }
    }
}

fn random_uuid_v4() -> Result<Uuid, OtpError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| OtpError::Internal)?;
    Ok(Builder::from_random_bytes(bytes).into_uuid())
}
