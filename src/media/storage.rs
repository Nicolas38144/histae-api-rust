use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type StorageFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ObjectStorageError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStorageError;

impl fmt::Display for ObjectStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("object_storage_unavailable")
    }
}

impl std::error::Error for ObjectStorageError {}

pub trait PhotoObjectStorage: Send + Sync {
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: Vec<u8>,
        content_type: &'static str,
        cache_control: &'static str,
    ) -> StorageFuture<'a, ()>;
    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()>;
    fn signed_get_url<'a>(&'a self, key: &'a str, ttl_seconds: u32) -> StorageFuture<'a, String>;
    fn check(&self) -> StorageFuture<'_, ()>;
}
