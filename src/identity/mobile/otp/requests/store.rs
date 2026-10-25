use std::{future::Future, pin::Pin};

use uuid::Uuid;

use super::{NewOtpRequest, PendingOtpRequest, RequestError};
use crate::infra::postgres::DatabaseError;

pub type RequestFuture<'a, T, E = DatabaseError> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

pub trait OtpRequestStore: Send + Sync {
    /// Returns the remaining cooldown, retaining the original deadline on replay.
    fn enqueue(&self, request: NewOtpRequest) -> RequestFuture<'_, u64, RequestError>;
    fn pending(&self, id: Uuid) -> RequestFuture<'_, Option<PendingOtpRequest>>;
    fn finish(&self, id: Uuid) -> RequestFuture<'_, ()>;
}
