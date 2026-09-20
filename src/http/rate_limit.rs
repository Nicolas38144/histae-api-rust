use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::{LimitPolicy, SecretString};
use crate::infra::redis::{FixedWindowIncrement, RedisService};

use super::error::ApiError;

pub type StoreFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FixedWindowIncrement, StoreError>> + Send + 'a>>;

pub trait FixedWindowStore: Send + Sync {
    fn increment<'a>(&'a self, key: &'a str, window: Duration) -> StoreFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreError;

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("rate_limit_store_unavailable")
    }
}

impl std::error::Error for StoreError {}

impl FixedWindowStore for RedisService {
    fn increment<'a>(&'a self, key: &'a str, window: Duration) -> StoreFuture<'a> {
        Box::pin(async move {
            self.increment_fixed_window(key, window)
                .await
                .map_err(|_| StoreError)
        })
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    key: Arc<[u8]>,
    backend: Backend,
}

#[derive(Clone)]
enum Backend {
    Memory(Arc<Mutex<MemoryState>>),
    External(Arc<dyn FixedWindowStore>),
}

#[derive(Default)]
struct MemoryState {
    entries: HashMap<String, MemoryEntry>,
    operations_since_sweep: u16,
}

struct MemoryEntry {
    count: u64,
    expires_at: Instant,
}

impl RateLimiter {
    pub fn memory(key: &SecretString) -> Self {
        Self {
            key: Arc::from(key.expose_secret().as_bytes()),
            backend: Backend::Memory(Arc::new(Mutex::new(MemoryState::default()))),
        }
    }

    pub fn redis(key: &SecretString, redis: RedisService) -> Self {
        Self::with_store(key, Arc::new(redis))
    }

    pub fn with_store(key: &SecretString, store: Arc<dyn FixedWindowStore>) -> Self {
        Self {
            key: Arc::from(key.expose_secret().as_bytes()),
            backend: Backend::External(store),
        }
    }

    pub async fn enforce(
        &self,
        name: &str,
        identity: &str,
        policy: &LimitPolicy,
        error_code: &'static str,
    ) -> Result<(), ApiError> {
        let storage_key = self.storage_key(name, identity)?;
        let retry_after = match &self.backend {
            Backend::Memory(state) => memory_increment(state, storage_key, policy)?,
            Backend::External(store) => {
                let result = store
                    .increment(&storage_key, policy.window)
                    .await
                    .map_err(|_| unavailable())?;
                (result.count > policy.max).then_some(result.ttl_millis)
            }
        };
        if let Some(milliseconds) = retry_after {
            let positive = milliseconds.max(1) as u64;
            let seconds = positive.div_ceil(1_000).max(1);
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                error_code,
                "Too many requests were sent. Please try again later.",
            )
            .with_retry_after(seconds));
        }
        Ok(())
    }

    fn storage_key(&self, name: &str, identity: &str) -> Result<String, ApiError> {
        let mut hmac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| unavailable())?;
        hmac.update(name.as_bytes());
        hmac.update(b":");
        hmac.update(identity.as_bytes());
        let digest = hmac.finalize().into_bytes();
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut hex, "{byte:02x}").map_err(|_| unavailable())?;
        }
        Ok(format!("histae:rate-limit:{name}:{hex}"))
    }
}

fn memory_increment(
    state: &Mutex<MemoryState>,
    key: String,
    policy: &LimitPolicy,
) -> Result<Option<i64>, ApiError> {
    let now = Instant::now();
    let mut state = state.lock().map_err(|_| unavailable())?;
    state.operations_since_sweep += 1;
    if state.operations_since_sweep >= 256 {
        state.operations_since_sweep = 0;
        state.entries.retain(|_, entry| entry.expires_at > now);
    }
    let entry = state.entries.entry(key).or_insert_with(|| MemoryEntry {
        count: 0,
        expires_at: now + policy.window,
    });
    if now >= entry.expires_at {
        *entry = MemoryEntry {
            count: 0,
            expires_at: now + policy.window,
        };
    }
    entry.count = entry.count.saturating_add(1);
    if entry.count <= policy.max {
        return Ok(None);
    }
    let remaining = entry.expires_at.saturating_duration_since(now);
    Ok(Some(
        i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX),
    ))
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "rate_limit_unavailable",
        "Request protection is temporarily unavailable.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretString {
        SecretString::new("0123456789abcdef0123456789abcdef".to_owned())
    }

    #[tokio::test]
    async fn memory_window_matches_the_nest_limit_and_retry_after_contract() {
        let limiter = RateLimiter::memory(&key());
        let policy = LimitPolicy {
            max: 1,
            window: Duration::from_millis(10_000),
        };
        assert_eq!(
            limiter
                .enforce(
                    "messages",
                    "raw-user-id",
                    &policy,
                    "message_rate_limit_exceeded"
                )
                .await,
            Ok(())
        );
        let error = limiter
            .enforce(
                "messages",
                "raw-user-id",
                &policy,
                "message_rate_limit_exceeded",
            )
            .await
            .expect_err("second request must be limited");
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error.code(), "message_rate_limit_exceeded");
    }

    #[test]
    fn hashes_the_identity_before_constructing_the_storage_key() {
        let limiter = RateLimiter::memory(&key());
        let storage_key = limiter
            .storage_key("messages", "raw-user-id")
            .expect("fixture key is valid");
        assert!(storage_key.starts_with("histae:rate-limit:messages:"));
        assert!(!storage_key.contains("raw-user-id"));
        assert_eq!(storage_key.len(), "histae:rate-limit:messages:".len() + 64);
    }
}
