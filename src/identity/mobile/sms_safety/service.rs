use std::sync::Arc;

use uuid::Uuid;

use super::{
    SmsControlError, SmsPermitFuture, SmsSafetySnapshot, SmsSafetyStore, SmsSendGuard,
    SmsSendRejection,
};
use crate::{
    config::{SecretString, SmsSafetyConfig},
    infra::{crypto::hmac_sha256, postgres::DatabaseError},
};

#[derive(Clone)]
pub struct SmsSafetyService {
    store: Arc<dyn SmsSafetyStore>,
    policy: SmsSafetyConfig,
    hash_key: SecretString,
    enabled: bool,
}

impl SmsSafetyService {
    pub fn new(
        store: Arc<dyn SmsSafetyStore>,
        policy: SmsSafetyConfig,
        hash_key: SecretString,
        enabled: bool,
    ) -> Self {
        Self {
            store,
            policy,
            hash_key,
            enabled,
        }
    }

    pub async fn snapshot(&self) -> Result<SmsSafetySnapshot, DatabaseError> {
        let usage = self.store.usage().await?;
        // The absolute threshold also covers a cold start. The relative test
        // requires a non-trivial baseline and at least five recent attempts.
        let traffic_spike = usage.last_5_minutes >= i64::from(self.policy.spike_per_5_minutes)
            || (usage.previous_hour >= 12
                && usage.last_5_minutes >= 5
                && usage.last_5_minutes.saturating_mul(12)
                    >= usage.previous_hour.saturating_mul(4));
        Ok(SmsSafetySnapshot {
            provider_enabled: self.enabled,
            suspended: self.policy.suspended || usage.manual_suspended,
            manual_suspended: usage.manual_suspended,
            environment_suspended: self.policy.suspended,
            version: usage.version,
            hourly_limit_per_phone: self.policy.hourly_per_phone,
            daily_limit_per_phone: self.policy.daily_per_phone,
            sends_last_5_minutes: usage.last_5_minutes,
            sends_last_hour: usage.last_hour,
            sends_last_24_hours: usage.last_day,
            spike_threshold_5_minutes: self.policy.spike_per_5_minutes,
            traffic_spike,
            updated_at: usage.updated_at,
        })
    }

    pub async fn set_suspended(
        &self,
        actor: Uuid,
        suspended: bool,
        version: i64,
    ) -> Result<(), SmsControlError> {
        self.store.set_suspended(actor, suspended, version).await
    }
}

impl SmsSendGuard for SmsSafetyService {
    fn reserve<'a>(&'a self, delivery_id: Uuid, phone: &'a str) -> SmsPermitFuture<'a> {
        Box::pin(async move {
            if self.policy.suspended {
                return Err(SmsSendRejection::Suspended);
            }
            let phone_hash =
                hmac_sha256(phone, &self.hash_key).map_err(|_| SmsSendRejection::Unavailable)?;
            self.store
                .reserve(delivery_id, phone_hash, self.policy.clone())
                .await
                .map_err(|_| SmsSendRejection::Unavailable)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::mobile::sms_safety::{SmsSafetyFuture, SmsUsage};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Store {
        usage: Mutex<SmsUsage>,
        calls: AtomicUsize,
    }

    impl SmsSafetyStore for Store {
        fn reserve<'a>(
            &'a self,
            _: Uuid,
            phone_hash: String,
            _: SmsSafetyConfig,
        ) -> SmsSafetyFuture<'a, Result<(), SmsSendRejection>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(phone_hash.len(), 64);
            assert!(!phone_hash.contains('+'));
            Box::pin(async { Err(DatabaseError::ConnectionFailed) })
        }
        fn usage(&self) -> SmsSafetyFuture<'_, SmsUsage> {
            let usage = self.usage.lock().expect("usage").clone();
            Box::pin(async { Ok(usage) })
        }
        fn set_suspended(
            &self,
            _: Uuid,
            _: bool,
            _: i64,
        ) -> SmsSafetyFuture<'_, (), SmsControlError> {
            Box::pin(async { Err(SmsControlError::Forbidden) })
        }
    }

    fn store() -> Arc<Store> {
        Arc::new(Store {
            usage: Mutex::new(SmsUsage {
                manual_suspended: false,
                version: 0,
                last_5_minutes: 0,
                last_hour: 0,
                last_day: 0,
                previous_hour: 0,
                updated_at: None,
            }),
            calls: AtomicUsize::new(0),
        })
    }

    #[tokio::test]
    async fn detects_absolute_and_relative_spikes_without_a_noisy_empty_baseline() {
        let store = store();
        let service = SmsSafetyService::new(
            store.clone(),
            Default::default(),
            SecretString::new("h".repeat(32)),
            true,
        );
        for (recent, baseline, expected) in [
            (4, 0, false),
            (19, 0, false),
            (20, 0, true),
            (5, 12, true),
            (5, 24, false),
        ] {
            {
                let mut usage = store.usage.lock().expect("usage");
                usage.last_5_minutes = recent;
                usage.previous_hour = baseline;
            }
            assert_eq!(
                service.snapshot().await.expect("snapshot").traffic_spike,
                expected
            );
        }
    }

    #[tokio::test]
    async fn forced_suspension_never_needs_the_store_and_database_failure_never_allows_sending() {
        let store = store();
        let forced = SmsSafetyService::new(
            store.clone(),
            SmsSafetyConfig {
                suspended: true,
                ..Default::default()
            },
            SecretString::new("h".repeat(32)),
            true,
        );
        assert_eq!(
            forced.reserve(Uuid::new_v4(), "+33600000000").await,
            Err(SmsSendRejection::Suspended)
        );
        assert_eq!(store.calls.load(Ordering::Relaxed), 0);
        let active = SmsSafetyService::new(
            store.clone(),
            Default::default(),
            SecretString::new("h".repeat(32)),
            true,
        );
        assert_eq!(
            active.reserve(Uuid::new_v4(), "+33600000000").await,
            Err(SmsSendRejection::Unavailable)
        );
        assert_eq!(store.calls.load(Ordering::Relaxed), 1);
    }
}
