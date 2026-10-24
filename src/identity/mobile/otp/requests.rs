//! Durable mobile OTP intents. HTTP performs no account lookup or provider call.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use crate::{
    config::SecretString,
    identity::mobile::account::MobileAccountStore,
    infra::{crypto::{open_secret, seal_secret}, postgres::DatabaseError},
    localization::Language,
    outbox::{types::{DispatchFailure, DispatchOutcome, OutboxEvent}, worker::{DispatchFuture, OutboxHandler}},
};
use uuid::Uuid;

use super::{OtpError, OtpService};

const ENCRYPTION_PURPOSE: &[u8] = b"histae:mobile-otp-request:v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    Conflict,
    Cooldown(u64),
    Database(DatabaseError),
}

impl From<DatabaseError> for RequestError {
    fn from(error: DatabaseError) -> Self { Self::Database(error) }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Conflict => "idempotency_key_conflict",
            Self::Cooldown(_) => "otp_resend_too_soon",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for RequestError {}

pub type RequestFuture<'a, T, E = DatabaseError> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

#[derive(Clone)]
pub struct NewOtpRequest {
    pub id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
    pub registration: bool,
    pub language: Language,
    pub ttl: Duration,
    pub cooldown: Duration,
}

pub struct PendingOtpRequest {
    pub id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
    pub registration: bool,
    pub language: Language,
}

pub trait OtpRequestStore: Send + Sync {
    /// Returns the remaining cooldown, retaining the original deadline on replay.
    fn enqueue(&self, request: NewOtpRequest) -> RequestFuture<'_, u64, RequestError>;
    fn pending(&self, id: Uuid) -> RequestFuture<'_, Option<PendingOtpRequest>>;
    fn finish(&self, id: Uuid) -> RequestFuture<'_, ()>;
}

#[derive(Clone)]
pub struct OtpRequestService {
    store: Arc<dyn OtpRequestStore>,
    encryption_key: SecretString,
    ttl: Duration,
    cooldown: Duration,
}

impl OtpRequestService {
    pub fn new(store: Arc<dyn OtpRequestStore>, encryption_key: SecretString, ttl: Duration, cooldown: Duration) -> Self {
        Self { store, encryption_key, ttl, cooldown }
    }

    pub async fn enqueue(&self, phone: &str, phone_hash: String, id: Uuid, registration: bool, language: Language) -> Result<u64, RequestError> {
        let purpose = purpose(id);
        let encrypted_phone = seal_secret(phone, &self.encryption_key, &purpose)
            .map_err(|_| RequestError::Database(DatabaseError::QueryFailed))?;
        self.store.enqueue(NewOtpRequest {
            id, phone_hash, encrypted_phone, registration, language,
            ttl: self.ttl, cooldown: self.cooldown,
        }).await
    }
}

pub struct OtpRequestHandler {
    store: Arc<dyn OtpRequestStore>,
    accounts: Arc<dyn MobileAccountStore>,
    otp: OtpService,
    encryption_key: SecretString,
}

impl OtpRequestHandler {
    pub fn new(store: Arc<dyn OtpRequestStore>, accounts: Arc<dyn MobileAccountStore>, otp: OtpService, encryption_key: SecretString) -> Self {
        Self { store, accounts, otp, encryption_key }
    }

    async fn process(&self, id: Uuid) -> Result<DispatchOutcome, DispatchFailure> {
        let request = self.store.pending(id).await.map_err(|_| unavailable())?;
        let Some(request) = request else {
            self.store.finish(id).await.map_err(|_| unavailable())?;
            return Ok(DispatchOutcome::Completed);
        };
        let phone = open_secret(&request.encrypted_phone, &self.encryption_key, &purpose(id))
            .map_err(|_| DispatchFailure::permanent("otp_request_invalid"))?;
        let account = self.accounts.find_by_phone_hash(request.phone_hash).await
            .map_err(|_| unavailable())?;
        let eligible = match (request.registration, account) {
            (true, None) => true,
            (false, Some(account)) => !account.is_banned,
            _ => false,
        };
        if eligible {
            let key = request.id.to_string();
            match self.otp.send_in(&phone, Some(&key), request.language).await {
                Ok(()) | Err(OtpError::DeliveryUnknown | OtpError::DeliveryUnavailable | OtpError::PhoneRateLimited) => {}
                // Stable UUID + existing OTP ledger prevent a duplicate provider
                // call when a worker retries or recovers an interrupted claim.
                Err(OtpError::Internal) => return Err(unavailable()),
                Err(_) => return Err(DispatchFailure::permanent("otp_request_invalid")),
            }
        }
        self.store.finish(id).await.map_err(|_| unavailable())?;
        Ok(DispatchOutcome::Completed)
    }
}

impl OutboxHandler for OtpRequestHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(self.process(event.aggregate_id))
    }
}

fn purpose(id: Uuid) -> Vec<u8> {
    [ENCRYPTION_PURPOSE, id.as_bytes()].concat()
}

fn unavailable() -> DispatchFailure { DispatchFailure::transient("otp_dispatch_unavailable") }
