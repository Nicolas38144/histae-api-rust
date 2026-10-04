use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{CreationResult, PhotoObject, ProcessingPhoto};
use crate::infra::postgres::DatabaseError;
use crate::media::codec::ProcessedPhoto;
use crate::moderation::domain::AutomatedPhotoModeration;

pub type PhotoStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait PhotoStore: Send + Sync {
    fn create_processing(&self, photo: ProcessingPhoto) -> PhotoStoreFuture<'_, CreationResult>;
    fn record_processed<'a>(
        &'a self,
        photo_id: Uuid,
        user_id: Uuid,
        photo: &'a ProcessedPhoto,
    ) -> PhotoStoreFuture<'a, bool>;
    fn activate(
        &self,
        photo_id: Uuid,
        user_id: Uuid,
        moderation: AutomatedPhotoModeration,
    ) -> PhotoStoreFuture<'_, bool>;
    fn begin_delete(&self, user_id: Uuid) -> PhotoStoreFuture<'_, bool>;
    fn begin_account_deletion(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> PhotoStoreFuture<'_, Vec<PhotoObject>>;
    fn find_deleting(&self, photo_id: Uuid) -> PhotoStoreFuture<'_, Option<PhotoObject>>;
    fn complete_deletion(&self, photo_id: Uuid) -> PhotoStoreFuture<'_, ()>;
    fn discard_processing(&self, photo_id: Uuid, user_id: Uuid) -> PhotoStoreFuture<'_, ()>;
}
