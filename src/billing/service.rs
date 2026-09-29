use std::fmt;
use std::sync::Arc;

use chrono::TimeDelta;
use uuid::{Uuid, Variant, Version};

use super::domain::{
    BeginCheckoutResult, BillingPeriod, CHECKOUT_CREATION_STALE_SECONDS, CHECKOUT_TTL_MINUTES,
    CUSTOMER_CREATE_SAFETY_HOURS, CheckoutSessionView, CustomerCreation, PersistedCheckoutSession,
    SubscriptionView,
};
use super::pg::{BeginCheckoutInput, BillingStore, BillingStoreError};
use super::stripe::{CheckoutInput, StripeGateway};
use crate::config::{BillingConfig, BillingProvider};
use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::{AccountActivityError, AccountActivityPool, ActivityLease};
use crate::shared::clock::Clock;
use crate::shared::text::javascript_trim;

const CUSTOMER_ERASURE_BATCH_SIZE: usize = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingError {
    InvalidIdempotencyKey,
    BillingUnavailable,
    AccountNotFound,
    SubscriptionAlreadyActive,
    CheckoutAlreadyInProgress,
    CustomerReconciliationRequired,
    IdempotencyKeyReused,
    IdempotencyKeyConsumed,
    BillingCustomerConflict,
    CheckoutStateConflict,
    StripeCheckoutUnavailable,
    StripeRequestFailed,
    BillingCustomerNotFound,
    ErasureStripeReconciliationRequired,
    DataErasureUnavailable,
    Database(DatabaseError),
    AccountActivity(AccountActivityError),
}

impl fmt::Display for BillingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdempotencyKey => "invalid_idempotency_key",
            Self::BillingUnavailable => "billing_unavailable",
            Self::AccountNotFound => "account_not_found",
            Self::SubscriptionAlreadyActive => "subscription_already_active",
            Self::CheckoutAlreadyInProgress => "checkout_already_in_progress",
            Self::CustomerReconciliationRequired => "billing_customer_reconciliation_required",
            Self::IdempotencyKeyReused => "idempotency_key_reused",
            Self::IdempotencyKeyConsumed => "idempotency_key_consumed",
            Self::BillingCustomerConflict => "billing_customer_conflict",
            Self::CheckoutStateConflict => "checkout_state_conflict",
            Self::StripeCheckoutUnavailable => "stripe_checkout_unavailable",
            Self::StripeRequestFailed => "stripe_request_failed",
            Self::BillingCustomerNotFound => "billing_customer_not_found",
            Self::ErasureStripeReconciliationRequired => "erasure_stripe_reconciliation_required",
            Self::DataErasureUnavailable => "data_erasure_unavailable",
            Self::Database(error) => error.safe_code(),
            Self::AccountActivity(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for BillingError {}

impl From<AccountActivityError> for BillingError {
    fn from(value: AccountActivityError) -> Self {
        Self::AccountActivity(value)
    }
}

#[derive(Clone)]
pub struct BillingService {
    store: Arc<dyn BillingStore>,
    stripe: Arc<dyn StripeGateway>,
    config: BillingConfig,
    activity: AccountActivityPool,
    clock: Arc<dyn Clock>,
}

impl BillingService {
    pub fn new(
        store: Arc<dyn BillingStore>,
        stripe: Arc<dyn StripeGateway>,
        config: BillingConfig,
        activity: AccountActivityPool,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            stripe,
            config,
            activity,
            clock,
        }
    }

    pub async fn subscription(&self, user_id: Uuid) -> Result<SubscriptionView, BillingError> {
        let (subscription, customer) = tokio::join!(
            self.store.subscription_for_user(user_id),
            self.store.customer_for_user(user_id)
        );
        let subscription = subscription.map_err(map_store_error)?;
        let customer = customer.map_err(map_store_error)?;
        let portal = self.config.provider == BillingProvider::Stripe && customer.is_some();
        Ok(match subscription {
            Some(row) => SubscriptionView::from_row(row, self.clock.now(), portal),
            None => SubscriptionView::free(portal),
        })
    }

    pub async fn create_checkout(
        &self,
        user_id: Uuid,
        billing_period: BillingPeriod,
        idempotency_key: Option<&str>,
    ) -> Result<CheckoutSessionView, BillingError> {
        let supplied_key = idempotency_key.map(str::to_owned);
        let service = self.clone();
        self.activity
            .run(&[user_id], move |lease| {
                Box::pin(async move {
                    service.ensure_enabled()?;
                    let idempotency_key = parse_idempotency_key(supplied_key.as_deref())?;
                    service
                        .create_checkout_while_active(
                            lease,
                            user_id,
                            billing_period,
                            idempotency_key,
                        )
                        .await
                })
            })
            .await
    }

    pub async fn create_portal(&self, user_id: Uuid) -> Result<String, BillingError> {
        self.ensure_enabled()?;
        let customer_id = self
            .store
            .customer_for_user(user_id)
            .await
            .map_err(map_store_error)?
            .ok_or(BillingError::BillingCustomerNotFound)?;
        let portal = self
            .stripe
            .create_portal_session(&customer_id, format!("histae-portal-{}", Uuid::new_v4()))
            .await
            .map_err(|_| BillingError::StripeRequestFailed)?;
        Ok(portal.url)
    }

    /// Deletes every Stripe customer known for an account. The caller owns the
    /// account-erasure lock; taking another activity lock here would deadlock.
    pub async fn delete_customer_for_account(&self, user_id: Uuid) -> Result<bool, BillingError> {
        let creations = self
            .store
            .customer_creations_for_erasure(user_id)
            .await
            .map_err(map_erasure_store_error)?;
        let linked = self
            .store
            .customer_for_user(user_id)
            .await
            .map_err(map_erasure_store_error)?;
        if creations.is_empty() && linked.is_none() {
            return Ok(true);
        }
        if self.config.provider != BillingProvider::Stripe {
            return Err(BillingError::DataErasureUnavailable);
        }

        for creation in &creations {
            let customer_id = self.resolve_customer_creation(creation).await?;
            self.delete_customer_confirmed(
                &customer_id,
                format!("histae-delete-orphan-{}", creation.id),
            )
            .await?;
            self.store
                .mark_created_customer_erased(creation.id)
                .await
                .map_err(map_erasure_store_error)?;
        }
        if creations.len() == CUSTOMER_ERASURE_BATCH_SIZE {
            return Ok(false);
        }
        if let Some(customer_id) = linked {
            self.delete_customer_confirmed(
                &customer_id,
                format!("histae-delete-customer-{user_id}"),
            )
            .await?;
        }
        Ok(true)
    }

    async fn create_checkout_while_active(
        &self,
        lease: &ActivityLease,
        user_id: Uuid,
        billing_period: BillingPeriod,
        idempotency_key: Uuid,
    ) -> Result<CheckoutSessionView, BillingError> {
        let now = self.clock.now();
        let expires_at = now + TimeDelta::minutes(CHECKOUT_TTL_MINUTES);
        let stale_before = now - TimeDelta::seconds(CHECKOUT_CREATION_STALE_SECONDS);
        let context = match self
            .store
            .begin_checkout(BeginCheckoutInput {
                user_id,
                idempotency_key,
                billing_period,
                attempt_id: Uuid::new_v4(),
                now,
                expires_at,
                stale_before,
            })
            .await
            .map_err(map_store_error)?
        {
            BeginCheckoutResult::Created(context) | BeginCheckoutResult::Retry(context) => context,
            BeginCheckoutResult::Replay(session) => return Ok(session.into()),
            BeginCheckoutResult::NotFound => return Err(BillingError::AccountNotFound),
            BeginCheckoutResult::AlreadySubscribed => {
                return Err(BillingError::SubscriptionAlreadyActive);
            }
            BeginCheckoutResult::InProgress => return Err(BillingError::CheckoutAlreadyInProgress),
            BeginCheckoutResult::CustomerReconciliationRequired => {
                return Err(BillingError::CustomerReconciliationRequired);
            }
            BeginCheckoutResult::IdempotencyConflict => {
                return Err(BillingError::IdempotencyKeyReused);
            }
            BeginCheckoutResult::IdempotencyConsumed => {
                return Err(BillingError::IdempotencyKeyConsumed);
            }
        };

        let attempt_id = context.attempt_id;
        let mut created_session_id = None;
        let mut unattached_customer_id = None;
        let result = async {
            let customer_id = match context.stripe_customer_id {
                Some(customer_id) => customer_id,
                None => {
                    let creation = self
                        .store
                        .begin_customer_creation(attempt_id)
                        .await
                        .map_err(map_store_error)?;
                    lease.assert_held()?;
                    let customer_id = self
                        .resolve_customer_creation_with_lease(lease, &creation)
                        .await?;
                    unattached_customer_id = Some(customer_id.clone());
                    if !self
                        .store
                        .save_customer(user_id, customer_id.clone())
                        .await
                        .map_err(map_store_error)?
                    {
                        return Err(BillingError::BillingCustomerConflict);
                    }
                    unattached_customer_id = None;
                    customer_id
                }
            };

            lease.assert_held()?;
            let price_id = match billing_period {
                BillingPeriod::Monthly => self.config.premium_monthly_price_id.clone(),
                BillingPeriod::Annual => self.config.premium_annual_price_id.clone(),
            };
            let session = self
                .stripe
                .create_checkout_session(CheckoutInput {
                    user_id,
                    customer_id,
                    price_id,
                    billing_period,
                    trial_days: if context.trial_used {
                        0
                    } else {
                        context.trial_days
                    },
                    expires_at,
                    idempotency_key: format!("histae-checkout-{attempt_id}"),
                })
                .await
                .map_err(|_| BillingError::StripeRequestFailed)?;
            created_session_id = Some(session.id.clone());
            let url = session.url.ok_or(BillingError::StripeCheckoutUnavailable)?;
            let persisted = PersistedCheckoutSession {
                session_id: session.id,
                url,
                expires_at: session.expires_at,
            };
            if !self
                .store
                .mark_checkout_open(attempt_id, persisted.clone())
                .await
                .map_err(map_store_error)?
            {
                if self
                    .stripe
                    .expire_checkout_session(&persisted.session_id)
                    .await
                    .is_err()
                {
                    tracing::warn!(event_code = "billing_checkout_cleanup_failed");
                }
                return Err(BillingError::CheckoutStateConflict);
            }
            Ok(persisted.into())
        }
        .await;

        if result.is_err() {
            self.store
                .mark_checkout_failed(attempt_id)
                .await
                .map_err(map_store_error)?;
            if let Some(session_id) = created_session_id
                && self
                    .stripe
                    .expire_checkout_session(&session_id)
                    .await
                    .is_err()
            {
                tracing::warn!(event_code = "billing_checkout_cleanup_failed");
            }
            if let Some(customer_id) = unattached_customer_id {
                if self
                    .delete_customer_confirmed(
                        &customer_id,
                        format!("histae-customer-cleanup-{attempt_id}"),
                    )
                    .await
                    .is_ok()
                {
                    let _ = self.store.mark_created_customer_erased(attempt_id).await;
                } else {
                    tracing::warn!(event_code = "billing_customer_cleanup_failed");
                }
            }
        }
        result
    }

    async fn resolve_customer_creation_with_lease(
        &self,
        lease: &ActivityLease,
        creation: &CustomerCreation,
    ) -> Result<String, BillingError> {
        if creation.customer_erased_at.is_some() {
            return Err(BillingError::IdempotencyKeyConsumed);
        }
        if let Some(customer_id) = &creation.created_customer_id {
            return Ok(customer_id.clone());
        }
        if self.clock.now() - creation.customer_creation_started_at
            >= TimeDelta::hours(CUSTOMER_CREATE_SAFETY_HOURS)
        {
            return Err(BillingError::ErasureStripeReconciliationRequired);
        }
        lease.assert_held()?;
        let customer = self
            .stripe
            .create_customer(
                creation.user_id,
                creation.id,
                format!("histae-customer-{}", creation.id),
            )
            .await
            .map_err(|_| BillingError::StripeRequestFailed)?;
        self.store
            .record_created_customer(creation.id, customer.id.clone())
            .await
            .map_err(map_store_error)?;
        Ok(customer.id)
    }

    async fn resolve_customer_creation(
        &self,
        creation: &CustomerCreation,
    ) -> Result<String, BillingError> {
        if creation.customer_erased_at.is_some() {
            return Err(BillingError::DataErasureUnavailable);
        }
        if let Some(customer_id) = &creation.created_customer_id {
            return Ok(customer_id.clone());
        }
        if self.clock.now() - creation.customer_creation_started_at
            >= TimeDelta::hours(CUSTOMER_CREATE_SAFETY_HOURS)
        {
            return Err(BillingError::ErasureStripeReconciliationRequired);
        }
        let customer = self
            .stripe
            .create_customer(
                creation.user_id,
                creation.id,
                format!("histae-customer-{}", creation.id),
            )
            .await
            .map_err(|_| BillingError::DataErasureUnavailable)?;
        self.store
            .record_created_customer(creation.id, customer.id.clone())
            .await
            .map_err(map_erasure_store_error)?;
        Ok(customer.id)
    }

    async fn delete_customer_confirmed(
        &self,
        customer_id: &str,
        idempotency_key: String,
    ) -> Result<(), BillingError> {
        match self
            .stripe
            .delete_customer(customer_id, idempotency_key)
            .await
        {
            Ok(customer) if customer.id == customer_id && customer.deleted => Ok(()),
            Ok(_) => Err(BillingError::DataErasureUnavailable),
            Err(_) => match self.stripe.retrieve_customer(customer_id).await {
                Ok(customer) if customer.id == customer_id && customer.deleted => Ok(()),
                _ => Err(BillingError::DataErasureUnavailable),
            },
        }
    }

    fn ensure_enabled(&self) -> Result<(), BillingError> {
        (self.config.provider == BillingProvider::Stripe)
            .then_some(())
            .ok_or(BillingError::BillingUnavailable)
    }
}

fn parse_idempotency_key(value: Option<&str>) -> Result<Uuid, BillingError> {
    let value = value
        .map(javascript_trim)
        .filter(|value| value.len() == 36)
        .ok_or(BillingError::InvalidIdempotencyKey)?;
    let id = Uuid::parse_str(value).map_err(|_| BillingError::InvalidIdempotencyKey)?;
    if id.get_version() != Some(Version::Random)
        || id.get_variant() != Variant::RFC4122
        || !id.hyphenated().to_string().eq_ignore_ascii_case(value)
    {
        return Err(BillingError::InvalidIdempotencyKey);
    }
    Ok(id)
}

fn map_store_error(error: BillingStoreError) -> BillingError {
    match error {
        BillingStoreError::Database(error) => BillingError::Database(error),
        BillingStoreError::CheckoutCustomerConflict => BillingError::BillingCustomerConflict,
        BillingStoreError::InvalidStoredData | BillingStoreError::CheckoutAttemptMissing => {
            BillingError::Database(DatabaseError::QueryFailed)
        }
    }
}

fn map_erasure_store_error(error: BillingStoreError) -> BillingError {
    match error {
        BillingStoreError::Database(error) => BillingError::Database(error),
        _ => BillingError::DataErasureUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_canonical_uuid_v4_idempotency_keys() {
        let id = Uuid::new_v4();
        assert_eq!(
            parse_idempotency_key(Some(&id.hyphenated().to_string())),
            Ok(id)
        );
        assert_eq!(
            parse_idempotency_key(Some(&id.simple().to_string())),
            Err(BillingError::InvalidIdempotencyKey)
        );
        assert_eq!(
            parse_idempotency_key(None),
            Err(BillingError::InvalidIdempotencyKey)
        );
    }
}
