use std::{future::Future, pin::Pin};

use uuid::Uuid;

use super::{NewOtpRequest, OtpRequestReceipt, PendingOtpRequest, RequestError};
use crate::infra::postgres::DatabaseError;

pub type RequestFuture<'a, T, E = DatabaseError> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

pub trait OtpRequestStore: Send + Sync {
    /// An idempotent replay keeps the challenge and the original resend deadline.
    fn enqueue(&self, request: NewOtpRequest)
    -> RequestFuture<'_, OtpRequestReceipt, RequestError>;
    /// Persists the review signup/login intent without a phone payload or SMS outbox job.
    fn record_review(
        &self,
        _request: NewOtpRequest,
    ) -> RequestFuture<'_, OtpRequestReceipt, RequestError> {
        Box::pin(async { Err(RequestError::Database(DatabaseError::QueryFailed)) })
    }
    fn pending(&self, id: Uuid) -> RequestFuture<'_, Option<PendingOtpRequest>>;
    fn finish(&self, id: Uuid) -> RequestFuture<'_, ()>;
}
