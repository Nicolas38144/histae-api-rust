use super::{
    PurchaseIntent, StoreError, StoreEvents, StoreGateway, StoreOffer, StorePersistence,
    StoreProvider, StoreSnapshot,
};
use crate::{
    billing::domain::BillingPeriod, config::StoreBillingConfig, infra::crypto::sha256_hex,
};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct StoreBillingService {
    repository: Arc<dyn StorePersistence>,
    gateway: Arc<dyn StoreGateway>,
    config: StoreBillingConfig,
    events: Option<Arc<dyn StoreEvents>>,
}
impl StoreBillingService {
    pub fn new(
        repository: impl StorePersistence + 'static,
        gateway: Arc<dyn StoreGateway>,
        config: StoreBillingConfig,
    ) -> Self {
        Self {
            repository: Arc::new(repository),
            gateway,
            config,
            events: None,
        }
    }
    pub fn with_events(mut self, events: Arc<dyn StoreEvents>) -> Self {
        self.events = Some(events);
        self
    }
    fn enabled(&self, provider: StoreProvider) -> Result<(), StoreError> {
        if match provider {
            StoreProvider::AppStore => self.config.apple_enabled,
            StoreProvider::GooglePlay => self.config.google_enabled,
        } {
            Ok(())
        } else {
            Err(StoreError::Unavailable)
        }
    }
    pub async fn intent(
        &self,
        user: Uuid,
        provider: StoreProvider,
    ) -> Result<PurchaseIntent, StoreError> {
        self.enabled(provider)?;
        if !self.repository.purchase_allowed(user).await? {
            return Err(StoreError::AlreadySubscribed);
        }
        let account = self.repository.account(user).await?;
        let offers = self.products(provider)?;
        Ok(PurchaseIntent {
            provider,
            account_binding: match provider {
                StoreProvider::AppStore => account.id.to_string(),
                StoreProvider::GooglePlay => account.google_id,
            },
            offers,
        })
    }
    pub fn products(&self, provider: StoreProvider) -> Result<Vec<StoreOffer>, StoreError> {
        self.enabled(provider)?;
        let offers = [BillingPeriod::Monthly, BillingPeriod::Annual]
            .into_iter()
            .map(|billing_period| {
                let (product_id, base_plan_id) = match provider {
                    StoreProvider::AppStore => (
                        if billing_period == BillingPeriod::Monthly {
                            &self.config.apple_monthly_product
                        } else {
                            &self.config.apple_annual_product
                        },
                        None,
                    ),
                    StoreProvider::GooglePlay => (
                        &self.config.google_product,
                        Some(if billing_period == BillingPeriod::Monthly {
                            self.config.google_monthly_plan.clone()
                        } else {
                            self.config.google_annual_plan.clone()
                        }),
                    ),
                };
                StoreOffer {
                    billing_period,
                    product_id: product_id.clone(),
                    base_plan_id,
                }
            })
            .collect();
        Ok(offers)
    }
    pub async fn verify(
        &self,
        user: Option<Uuid>,
        provider: StoreProvider,
        reference: String,
    ) -> Result<(), StoreError> {
        self.enabled(provider)?;
        if reference.is_empty() || reference.len() > 4096 {
            return Err(StoreError::Invalid);
        }
        let observed = self.repository.observed_at().await?;
        let snapshot = self.gateway.verify(provider, reference.clone()).await?;
        self.validate(&snapshot, provider, &reference)?;
        if let Some(owner) = self
            .repository
            .apply(snapshot, reference, user, observed)
            .await?
        {
            if let Some(events) = &self.events {
                let _ = events.subscription_changed(owner).await;
            }
        }
        Ok(())
    }
    fn validate(
        &self,
        snapshot: &StoreSnapshot,
        provider: StoreProvider,
        reference: &str,
    ) -> Result<(), StoreError> {
        if !snapshot.valid()
            || snapshot.provider != provider
            || (self.config.production != (snapshot.environment == "production"))
        {
            return Err(StoreError::Invalid);
        }
        let valid = match provider {
            StoreProvider::AppStore => match snapshot.billing_period {
                BillingPeriod::Monthly => snapshot.product_id == self.config.apple_monthly_product,
                BillingPeriod::Annual => snapshot.product_id == self.config.apple_annual_product,
            },
            StoreProvider::GooglePlay => {
                snapshot.original_id == sha256_hex(reference.as_bytes())
                    && snapshot.product_id == self.config.google_product
                    && snapshot.base_plan_id.as_deref()
                        == Some(match snapshot.billing_period {
                            BillingPeriod::Monthly => &self.config.google_monthly_plan,
                            BillingPeriod::Annual => &self.config.google_annual_plan,
                        })
            }
        };
        if valid {
            Ok(())
        } else {
            Err(StoreError::Invalid)
        }
    }
    pub async fn notification(
        &self,
        provider: StoreProvider,
        body: serde_json::Value,
        authorization: Option<String>,
    ) -> Result<(), StoreError> {
        self.enabled(provider)?;
        let event = self
            .gateway
            .notification(provider, body, authorization)
            .await?;
        if event.event_id.is_empty()
            || event.event_id.len() > 128
            || event
                .reference
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 4096)
        {
            return Err(StoreError::Invalid);
        }
        self.repository
            .receive(provider, event.event_id, event.reference)
            .await
    }
    pub async fn process(&self, kind: &str, id: Uuid) -> Result<(), StoreError> {
        let notification = kind == "billing.store.notification";
        let Some((provider, reference, user)) = self.repository.reference(id, notification).await?
        else {
            return Ok(());
        };
        if kind == "billing.store.acknowledge" {
            self.enabled(provider)?;
            self.gateway.acknowledge(reference).await?;
            self.repository.acknowledged(id).await?;
        } else {
            // Account removal must never reattach a purchase or reopen access.
            if !notification && user.is_none() {
                return Ok(());
            }
            self.verify(user, provider, reference).await?;
            if notification {
                self.repository.complete_notification(id).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::stores::{StoreAccount, StoreFuture, StoreNotification};
    use crate::config::SecretString;
    use chrono::{DateTime, TimeDelta, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Persistence(Arc<AtomicUsize>);
    impl StorePersistence for Persistence {
        fn observed_at(&self) -> StoreFuture<'_, DateTime<Utc>> {
            Box::pin(async { Ok(Utc::now()) })
        }
        fn account(&self, _user: Uuid) -> StoreFuture<'_, StoreAccount> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn purchase_allowed(&self, _user: Uuid) -> StoreFuture<'_, bool> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn apply(
            &self,
            _snapshot: StoreSnapshot,
            _reference: String,
            expected: Option<Uuid>,
            _observed: DateTime<Utc>,
        ) -> StoreFuture<'_, Option<Uuid>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move { Ok(expected) })
        }
        fn receive(
            &self,
            _provider: StoreProvider,
            _event: String,
            _reference: Option<String>,
        ) -> StoreFuture<'_, ()> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn reference(
            &self,
            _id: Uuid,
            _notification: bool,
        ) -> StoreFuture<'_, Option<super::super::store::PurchaseReference>> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn complete_notification(&self, _id: Uuid) -> StoreFuture<'_, ()> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn acknowledged(&self, _id: Uuid) -> StoreFuture<'_, ()> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
    }
    struct Gateway(StoreSnapshot);
    impl StoreGateway for Gateway {
        fn verify(
            &self,
            _provider: StoreProvider,
            _reference: String,
        ) -> StoreFuture<'_, StoreSnapshot> {
            let snapshot = self.0.clone();
            Box::pin(async { Ok(snapshot) })
        }
        fn notification(
            &self,
            _provider: StoreProvider,
            _body: serde_json::Value,
            _authorization: Option<String>,
        ) -> StoreFuture<'_, StoreNotification> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
        fn acknowledge(&self, _reference: String) -> StoreFuture<'_, ()> {
            Box::pin(async { Err(StoreError::Unavailable) })
        }
    }
    fn config() -> StoreBillingConfig {
        StoreBillingConfig {
            apple_enabled: true,
            google_enabled: true,
            apple_monthly_product: "premium.monthly".into(),
            apple_annual_product: "premium.annual".into(),
            google_product: "premium".into(),
            google_monthly_plan: "monthly".into(),
            google_annual_plan: "annual".into(),
            verifier_url: url::Url::parse("http://store-verifier:8080").unwrap(),
            verifier_token: SecretString::new("synthetic-verifier-token".into()),
            production: false,
        }
    }
    fn snapshot() -> StoreSnapshot {
        StoreSnapshot {
            provider: StoreProvider::AppStore,
            original_id: "100".into(),
            transaction_id: "101".into(),
            product_id: "premium.monthly".into(),
            base_plan_id: None,
            account_binding: Uuid::new_v4().to_string(),
            environment: "sandbox".into(),
            billing_period: BillingPeriod::Monthly,
            status: "active".into(),
            starts_at: Utc::now() - TimeDelta::days(1),
            expires_at: Utc::now() + TimeDelta::days(30),
            auto_renew: true,
            acknowledgement_required: false,
            linked_reference_hash: None,
        }
    }
    #[tokio::test]
    async fn invalid_provider_metadata_never_reaches_persistence() {
        let mut invalid = Vec::new();
        let mut row = snapshot();
        row.environment = "production".into();
        invalid.push(row);
        let mut row = snapshot();
        row.product_id = "unrecognized".into();
        invalid.push(row);
        let mut row = snapshot();
        row.account_binding.clear();
        invalid.push(row);
        let mut row = snapshot();
        row.provider = StoreProvider::GooglePlay;
        invalid.push(row);
        let mut row = snapshot();
        row.status = "future_unknown_status".into();
        invalid.push(row);
        for snapshot in invalid {
            let writes = Arc::new(AtomicUsize::new(0));
            let service = StoreBillingService::new(
                Persistence(writes.clone()),
                Arc::new(Gateway(snapshot)),
                config(),
            );
            assert_eq!(
                service
                    .verify(Some(Uuid::new_v4()), StoreProvider::AppStore, "100".into())
                    .await,
                Err(StoreError::Invalid)
            );
            assert_eq!(writes.load(Ordering::Relaxed), 0);
        }
    }
    #[tokio::test]
    async fn a_verified_known_product_is_persisted_once() {
        let writes = Arc::new(AtomicUsize::new(0));
        let service = StoreBillingService::new(
            Persistence(writes.clone()),
            Arc::new(Gateway(snapshot())),
            config(),
        );
        service
            .verify(Some(Uuid::new_v4()), StoreProvider::AppStore, "100".into())
            .await
            .unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 1);
    }
}
