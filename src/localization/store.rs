use super::{Language, catalog::CatalogSnapshot};
use crate::infra::postgres::DatabaseError;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type LocalizationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait LocalizationStore: Send + Sync {
    fn language(&self, user_id: Uuid) -> LocalizationFuture<'_, Language>;
    fn set_language(&self, user_id: Uuid, language: Language) -> LocalizationFuture<'_, Language>;
    fn catalog(&self) -> LocalizationFuture<'_, CatalogSnapshot>;
}
