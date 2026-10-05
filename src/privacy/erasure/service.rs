use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::Serialize;
use uuid::{Uuid, Variant, Version};

use crate::billing::service::{BillingError, BillingService};
use crate::infra::crypto::sha256_hex;
use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::{AccountActivityError, AccountActivityPool, TryExclusive};
use crate::media::service::{PhotoDeletionHandler, PhotoService};
use crate::outbox::types::{DispatchFailure, DispatchOutcome, OutboxEvent};
use crate::outbox::worker::{DispatchFuture, OutboxHandler};
use crate::shared::clock::Clock;

const PHOTO_ERASURE_BATCH_SIZE: u32 = 50;
const SWIPE_ERASURE_BATCH_SIZE: u32 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IssuedDeletionToken {
    pub confirmation_token: String,
    pub expires_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptedErasureStatus {
    InProgress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AcceptedErasure {
    pub request_id: Uuid,
    pub status: AcceptedErasureStatus,
}

#[derive(Clone, Debug)]
pub struct NewDeletionToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountDeletionError {
    AccountNotFound,
    InvalidOrExpiredToken,
    RandomUnavailable,
    TimeOutOfRange,
    Database(DatabaseError),
}

impl fmt::Display for AccountDeletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AccountNotFound => "account_not_found",
            Self::InvalidOrExpiredToken => "invalid_or_expired_deletion_token",
            Self::RandomUnavailable => "deletion_token_random_unavailable",
            Self::TimeOutOfRange => "deletion_token_time_out_of_range",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for AccountDeletionError {}

impl From<DatabaseError> for AccountDeletionError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

pub type AccountDeletionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait AccountDeletionStore: Send + Sync {
    fn replace_token(&self, token: NewDeletionToken) -> AccountDeletionFuture<'_, bool>;

    fn accept(
        &self,
        user_id: Uuid,
        token_id: Uuid,
        token_hash: String,
        now: DateTime<Utc>,
    ) -> AccountDeletionFuture<'_, Option<AcceptedErasure>>;
}

#[derive(Clone)]
pub struct AccountDeletionService {
    store: Arc<dyn AccountDeletionStore>,
    token_ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl AccountDeletionService {
    pub fn new(
        store: Arc<dyn AccountDeletionStore>,
        token_ttl: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            token_ttl,
            clock,
        }
    }

    pub async fn issue(&self, user_id: Uuid) -> Result<IssuedDeletionToken, AccountDeletionError> {
        let token_id = Uuid::new_v4();
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_| AccountDeletionError::RandomUnavailable)?;
        let confirmation_token = format!(
            "{}:{}",
            token_id.hyphenated(),
            URL_SAFE_NO_PAD.encode(secret)
        );
        let ttl = ChronoDuration::from_std(self.token_ttl)
            .map_err(|_| AccountDeletionError::TimeOutOfRange)?;
        let expires_at = millisecond_time(self.clock.now())?
            .checked_add_signed(ttl)
            .ok_or(AccountDeletionError::TimeOutOfRange)?;
        let stored = self
            .store
            .replace_token(NewDeletionToken {
                id: token_id,
                user_id,
                token_hash: sha256_hex(confirmation_token.as_bytes()),
                expires_at,
            })
            .await?;
        if !stored {
            return Err(AccountDeletionError::AccountNotFound);
        }
        Ok(IssuedDeletionToken {
            confirmation_token,
            expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        })
    }

    pub async fn accept(
        &self,
        user_id: Uuid,
        confirmation_token: &str,
    ) -> Result<AcceptedErasure, AccountDeletionError> {
        let token_id = parse_deletion_token(confirmation_token)
            .ok_or(AccountDeletionError::InvalidOrExpiredToken)?;
        self.store
            .accept(
                user_id,
                token_id,
                sha256_hex(confirmation_token.as_bytes()),
                millisecond_time(self.clock.now())?,
            )
            .await?
            .ok_or(AccountDeletionError::InvalidOrExpiredToken)
    }
}

fn millisecond_time(value: DateTime<Utc>) -> Result<DateTime<Utc>, AccountDeletionError> {
    DateTime::from_timestamp_millis(value.timestamp_millis())
        .ok_or(AccountDeletionError::TimeOutOfRange)
}

pub fn valid_deletion_token(value: &str) -> bool {
    parse_deletion_token(value).is_some()
}

fn parse_deletion_token(value: &str) -> Option<Uuid> {
    if value.len() != 80 {
        return None;
    }
    let (id, secret) = value.split_once(':')?;
    if value[id.len() + 1..].contains(':')
        || secret.len() != 43
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    let parsed = Uuid::parse_str(id).ok()?;
    if parsed.get_version() != Some(Version::Random)
        || parsed.get_variant() != Variant::RFC4122
        || parsed.hyphenated().to_string() != id
    {
        return None;
    }
    Some(parsed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErasureStep {
    Stripe,
    Photos,
    Swipes,
    Postgres,
    Completed,
}

impl ErasureStep {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stripe => "stripe",
            Self::Photos => "photos",
            Self::Swipes => "swipes",
            Self::Postgres => "postgres",
            Self::Completed => "completed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stripe" => Some(Self::Stripe),
            "photos" => Some(Self::Photos),
            "swipes" => Some(Self::Swipes),
            "postgres" => Some(Self::Postgres),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    const fn unavailable_code(self) -> &'static str {
        match self {
            Self::Stripe => "erasure_stripe_unavailable",
            Self::Photos => "erasure_photos_unavailable",
            Self::Swipes => "erasure_swipes_unavailable",
            Self::Postgres | Self::Completed => "erasure_postgres_unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimedErasure {
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub step: ErasureStep,
}

pub type ErasureStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait ErasureStore: Send + Sync {
    fn claimed(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
    ) -> ErasureStoreFuture<'_, Option<ClaimedErasure>>;

    fn advance(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
        current: ClaimedErasure,
        next: ErasureStep,
    ) -> ErasureStoreFuture<'_, bool>;

    fn delete_swipe_batch(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
        current: ClaimedErasure,
        batch_size: u32,
    ) -> ErasureStoreFuture<'_, bool>;

    fn defer(&self, event_id: Uuid, worker_id: Uuid) -> ErasureStoreFuture<'_, ()>;
}

pub type ErasureDependencyFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, ErasureStepError>> + Send + 'a>>;

pub trait CustomerEraser: Send + Sync {
    fn delete_customer_for_account(&self, user_id: Uuid) -> ErasureDependencyFuture<'_>;
}

impl CustomerEraser for BillingService {
    fn delete_customer_for_account(&self, user_id: Uuid) -> ErasureDependencyFuture<'_> {
        Box::pin(async move {
            BillingService::delete_customer_for_account(self, user_id)
                .await
                .map_err(|error| match error {
                    BillingError::ErasureStripeReconciliationRequired => {
                        ErasureStepError::new("erasure_stripe_reconciliation_required")
                    }
                    _ => ErasureStepError::new("erasure_stripe_unavailable"),
                })
        })
    }
}

pub trait PhotoEraser: Send + Sync {
    fn delete_photos_for_account(&self, user_id: Uuid) -> ErasureDependencyFuture<'_>;
}

impl PhotoEraser for PhotoService {
    fn delete_photos_for_account(&self, user_id: Uuid) -> ErasureDependencyFuture<'_> {
        Box::pin(async move {
            self.delete_for_account(user_id, PHOTO_ERASURE_BATCH_SIZE)
                .await
                .map_err(|_| ErasureStepError::new("erasure_photos_unavailable"))
        })
    }
}

impl PhotoEraser for PhotoDeletionHandler {
    fn delete_photos_for_account(&self, user_id: Uuid) -> ErasureDependencyFuture<'_> {
        Box::pin(async move {
            self.delete_for_account(user_id, PHOTO_ERASURE_BATCH_SIZE)
                .await
                .map_err(|_| ErasureStepError::new("erasure_photos_unavailable"))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErasureStepError {
    pub code: &'static str,
}

impl ErasureStepError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }
}

impl fmt::Display for ErasureStepError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ErasureStepError {}

impl From<AccountActivityError> for ErasureStepError {
    fn from(_: AccountActivityError) -> Self {
        Self::new("erasure_postgres_unavailable")
    }
}

#[derive(Clone)]
pub struct ErasureService {
    store: Arc<dyn ErasureStore>,
    activity: AccountActivityPool,
    billing: Arc<dyn CustomerEraser>,
    photos: Arc<dyn PhotoEraser>,
}

impl ErasureService {
    pub fn new(
        store: Arc<dyn ErasureStore>,
        activity: AccountActivityPool,
        billing: Arc<dyn CustomerEraser>,
        photos: Arc<dyn PhotoEraser>,
    ) -> Self {
        Self {
            store,
            activity,
            billing,
            photos,
        }
    }

    pub async fn process(&self, event_id: Uuid, worker_id: Uuid) -> Result<bool, ErasureStepError> {
        let initial = self
            .store
            .claimed(event_id, worker_id)
            .await
            .map_err(|_| ErasureStepError::new("erasure_invalid_state"))?
            .ok_or_else(|| ErasureStepError::new("erasure_invalid_state"))?;
        if initial.step == ErasureStep::Completed {
            return Ok(true);
        }

        let store = Arc::clone(&self.store);
        let deferred_store = Arc::clone(&self.store);
        let billing = Arc::clone(&self.billing);
        let photos = Arc::clone(&self.photos);
        let step = initial.step;
        let result: Result<TryExclusive<bool>, ErasureStepError> = self
            .activity
            .try_exclusive(initial.user_id, move |lease| {
                Box::pin(async move {
                    let Some(current) = store
                        .claimed(event_id, worker_id)
                        .await
                        .map_err(|_| ErasureStepError::new(step.unavailable_code()))?
                    else {
                        return Ok(false);
                    };
                    if current.step == ErasureStep::Completed {
                        return Ok(true);
                    }
                    let next = match current.step {
                        ErasureStep::Stripe => {
                            if billing.delete_customer_for_account(current.user_id).await? {
                                ErasureStep::Photos
                            } else {
                                ErasureStep::Stripe
                            }
                        }
                        ErasureStep::Photos => {
                            if photos.delete_photos_for_account(current.user_id).await? {
                                ErasureStep::Swipes
                            } else {
                                ErasureStep::Photos
                            }
                        }
                        ErasureStep::Swipes => {
                            store
                                .delete_swipe_batch(
                                    event_id,
                                    worker_id,
                                    current,
                                    SWIPE_ERASURE_BATCH_SIZE,
                                )
                                .await
                                .map_err(|_| ErasureStepError::new("erasure_swipes_unavailable"))?;
                            lease
                                .assert_held()
                                .map_err(|_| ErasureStepError::new("erasure_swipes_unavailable"))?;
                            return Ok(false);
                        }
                        ErasureStep::Postgres => ErasureStep::Completed,
                        ErasureStep::Completed => return Ok(true),
                    };
                    lease
                        .assert_held()
                        .map_err(|_| ErasureStepError::new(current.step.unavailable_code()))?;
                    let advanced = store
                        .advance(event_id, worker_id, current, next)
                        .await
                        .map_err(|_| ErasureStepError::new(current.step.unavailable_code()))?;
                    Ok(advanced && next == ErasureStep::Completed)
                })
            })
            .await;
        // Dependency errors are already normalized. In particular an uncertain
        // Stripe creation needs reconciliation, not an ordinary provider retry.
        let result = result?;

        match result {
            TryExclusive::Acquired(completed) => Ok(completed),
            TryExclusive::NotAcquired => {
                deferred_store
                    .defer(event_id, worker_id)
                    .await
                    .map_err(|_| ErasureStepError::new(step.unavailable_code()))?;
                Ok(false)
            }
        }
    }
}

impl OutboxHandler for ErasureService {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(async move {
            self.process(event.id, worker_id)
                .await
                .map(|completed| {
                    if completed {
                        DispatchOutcome::Completed
                    } else {
                        DispatchOutcome::Deferred
                    }
                })
                .map_err(|error| DispatchFailure::transient(error.code))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_canonical_uuid_v4_deletion_tokens() {
        let id = Uuid::new_v4();
        let secret = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let token = format!("{}:{secret}", id.hyphenated());
        assert!(valid_deletion_token(&token));
        assert!(!valid_deletion_token("delete-me"));
        assert!(!valid_deletion_token(&format!("{}:{secret}", id.simple())));
        assert!(!valid_deletion_token(&format!("{}:short", id.hyphenated())));
    }
}
