use super::domain::*;
use crate::infra::postgres::DatabaseError;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type OutboxAdminFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait OutboxAdminStore: Send + Sync {
    fn list_dead_letters(
        &self,
        limit: u32,
        cursor: Option<DeadLetterCursor>,
    ) -> OutboxAdminFuture<'_, Vec<DeadLetterRow>>;

    fn retry_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult>;

    fn discard_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult>;
}
