use super::{BillingReconciliationService, BillingReconciliationStore, ReconciliationStoreError};
use crate::config::{BillingConfig, BillingProvider};
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};
use crate::outbox::{
    types::{DispatchFailure, DispatchOutcome, OutboxEvent},
    worker::{DispatchFuture, OutboxHandler},
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Clone)]
pub struct BillingReconciliationHandler {
    service: BillingReconciliationService,
}

impl BillingReconciliationHandler {
    pub fn new(service: BillingReconciliationService) -> Self {
        Self { service }
    }
}

impl OutboxHandler for BillingReconciliationHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(async move {
            self.service
                .process(&event.event_type, event.aggregate_id)
                .await
                .map(|()| DispatchOutcome::Completed)
                .map_err(|error| DispatchFailure {
                    code: error.code,
                    permanent: error.permanent,
                })
        })
    }
}

#[derive(Clone)]
pub struct BillingReconciliationScheduler {
    store: Arc<dyn BillingReconciliationStore>,
    tracker: MaintenanceTracker,
    config: Arc<BillingConfig>,
}

impl BillingReconciliationScheduler {
    pub fn new(
        store: Arc<dyn BillingReconciliationStore>,
        tracker: MaintenanceTracker,
        config: BillingConfig,
    ) -> Self {
        Self {
            store,
            tracker,
            config: Arc::new(config),
        }
    }

    pub async fn run_once(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<u32>, ReconciliationStoreError> {
        if self.config.provider != BillingProvider::Stripe {
            return Ok(None);
        }
        self.tracker
            .track(
                MaintenanceJobName::Billing,
                async {
                    self.store
                        .schedule_due(now, self.config.reconciliation_batch_size)
                        .await
                        .map(Some)
                },
                |count| MaintenanceProgress {
                    processed_count: u64::from(*count),
                    batch_count: u32::from(*count > 0),
                    work_remaining: *count == u32::from(self.config.reconciliation_batch_size),
                },
                |_| "billing_reconciliation_schedule_failed",
            )
            .await
    }
}
