use super::{StoreBillingService, StoreError};
use crate::outbox::{
    types::{DispatchFailure, DispatchOutcome, OutboxEvent},
    worker::{DispatchFuture, OutboxHandler},
};
use uuid::Uuid;

impl OutboxHandler for StoreBillingService {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(async move {
            self.process(event.event_type.as_str(), event.aggregate_id)
                .await
                .map(|()| DispatchOutcome::Completed)
                .map_err(|error| DispatchFailure {
                    code: match error {
                        StoreError::Ownership => "store_purchase_account_conflict",
                        StoreError::Invalid => "invalid_store_purchase",
                        StoreError::AccountUnavailable => "account_not_found",
                        _ => "store_verification_unavailable",
                    },
                    permanent: matches!(
                        error,
                        StoreError::Ownership
                            | StoreError::Invalid
                            | StoreError::AccountUnavailable
                    ),
                })
        })
    }
}
