use super::{
    domain::SubscriptionView,
    store::{BillingStore, BillingStoreError},
};
use crate::{infra::postgres::DatabaseError, shared::clock::Clock};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingError {
    InvalidStoredData,
    Database(DatabaseError),
}
#[derive(Clone)]
pub struct BillingService {
    store: Arc<dyn BillingStore>,
    clock: Arc<dyn Clock>,
}

impl BillingService {
    pub fn new(store: Arc<dyn BillingStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }
    pub async fn subscription(&self, user_id: Uuid) -> Result<SubscriptionView, BillingError> {
        self.store
            .effective_subscription_for_user(user_id, self.clock.now())
            .await
            .map(|view| view.unwrap_or_else(SubscriptionView::free))
            .map_err(|error| match error {
                BillingStoreError::Database(error) => BillingError::Database(error),
                BillingStoreError::InvalidStoredData => BillingError::InvalidStoredData,
            })
    }
}
