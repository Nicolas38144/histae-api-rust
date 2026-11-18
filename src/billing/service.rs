use super::{
    domain::{BillingError, SubscriptionView},
    store::BillingStore,
};
use crate::shared::clock::Clock;
use std::sync::Arc;
use uuid::Uuid;

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
    }
}
