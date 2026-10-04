use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::postgres::DatabaseError;
use crate::outbox::types::{ClaimWindow, OutboxEvent, RetryResult};

pub type OutboxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait OutboxStore: Send + Sync {
    fn claim_batch<'a>(
        &'a self,
        worker_id: Uuid,
        window: ClaimWindow,
        limit: u32,
    ) -> OutboxFuture<'a, Vec<OutboxEvent>>;

    fn renew_claim<'a>(&'a self, event_id: Uuid, worker_id: Uuid) -> OutboxFuture<'a, bool>;

    fn complete<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        processed_at: DateTime<Utc>,
    ) -> OutboxFuture<'a, bool>;

    fn reschedule<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        available_at: DateTime<Utc>,
        error_code: &'static str,
        max_attempts: u16,
    ) -> OutboxFuture<'a, RetryResult>;

    fn purge_resolved<'a>(&'a self, before: DateTime<Utc>, limit: u32) -> OutboxFuture<'a, u64>;
}
