use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use chrono::TimeDelta;
use regex::Regex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::domain::{
    CreationResult, PHOTO_CACHE_CONTROL, PHOTO_IDEMPOTENCY_HOURS, PHOTO_URL_TTL_SECONDS,
    ProcessingPhoto, UploadPhoto, UploadResult,
};
use super::pg::PhotoStore;
use super::storage::PhotoObjectStorage;
use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::{AccountActivityError, AccountActivityPool, ActivityLease};
use crate::moderation::photo::PhotoModerator;
use crate::outbox::types::{DispatchFailure, DispatchOutcome, OutboxEvent};
use crate::outbox::worker::{DispatchFuture, OutboxHandler};
use crate::photo_codec_probe::{
    InvalidPhotoReason, PhotoCodecError, PhotoCodecProbe, ProcessedPhoto, UploadedPhoto,
};
use crate::profiles::service::{ProfilePhotoUrlFuture, ProfilePhotoUrlProvider};
use crate::shared::clock::Clock;

pub type ProcessorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProcessedPhoto, PhotoCodecError>> + Send + 'a>>;

pub trait PhotoProcessor: Send + Sync {
    fn process<'a>(&'a self, upload: &'a UploadPhoto) -> ProcessorFuture<'a>;
}

impl PhotoProcessor for PhotoCodecProbe {
    fn process<'a>(&'a self, upload: &'a UploadPhoto) -> ProcessorFuture<'a> {
        Box::pin(async move {
            self.to_webp(UploadedPhoto {
                filename: &upload.filename,
                mime_type: &upload.mime_type,
                body: &upload.body,
            })
            .await
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhotoError {
    ProfileNotFound,
    UpdateInProgress,
    IdempotencyConflict,
    IdempotencyConsumed,
    InvalidPhoto(InvalidPhotoReason),
    PhotoTooLarge,
    UpdateConflict,
    StorageUnavailable,
    AccountActivity(AccountActivityError),
    Database(DatabaseError),
}

impl From<AccountActivityError> for PhotoError {
    fn from(error: AccountActivityError) -> Self {
        Self::AccountActivity(error)
    }
}

impl From<DatabaseError> for PhotoError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone)]
pub struct PhotoService {
    store: Arc<dyn PhotoStore>,
    processor: Arc<dyn PhotoProcessor>,
    storage: Arc<dyn PhotoObjectStorage>,
    moderation: Arc<dyn PhotoModerator>,
    activity: AccountActivityPool,
    clock: Arc<dyn Clock>,
}

impl PhotoService {
    pub fn new(
        store: Arc<dyn PhotoStore>,
        processor: Arc<dyn PhotoProcessor>,
        storage: Arc<dyn PhotoObjectStorage>,
        moderation: Arc<dyn PhotoModerator>,
        activity: AccountActivityPool,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            processor,
            storage,
            moderation,
            activity,
            clock,
        }
    }

    pub async fn upload(
        &self,
        user_id: Uuid,
        upload: UploadPhoto,
        idempotency_key: Uuid,
    ) -> Result<UploadResult, PhotoError> {
        let service = self.clone();
        self.activity
            .run(&[user_id], move |lease| {
                Box::pin(async move {
                    service
                        .upload_while_active(user_id, upload, idempotency_key, lease)
                        .await
                })
            })
            .await
    }

    async fn upload_while_active(
        &self,
        user_id: Uuid,
        upload: UploadPhoto,
        idempotency_key: Uuid,
        lease: &ActivityLease,
    ) -> Result<UploadResult, PhotoError> {
        let photo_id = Uuid::new_v4();
        let object_key = format!("profile-photos/{user_id}/{photo_id}.webp");
        let created_at = self.clock.now();
        let expires_at = created_at + TimeDelta::hours(PHOTO_IDEMPOTENCY_HOURS);
        let creation = self
            .store
            .create_processing(ProcessingPhoto {
                id: photo_id,
                user_id,
                object_key: object_key.clone(),
                idempotency_key,
                request_sha256: upload_request_hash(&upload),
                created_at,
                expires_at,
            })
            .await?;
        match creation {
            CreationResult::ProfileNotFound => return Err(PhotoError::ProfileNotFound),
            CreationResult::UpdateInProgress => return Err(PhotoError::UpdateInProgress),
            CreationResult::IdempotencyConflict => return Err(PhotoError::IdempotencyConflict),
            CreationResult::IdempotencyConsumed => return Err(PhotoError::IdempotencyConsumed),
            CreationResult::Replay(photo) => {
                return Ok(UploadResult {
                    photo: self.sign(&photo.object_key).await?,
                    moderation_status: photo.moderation_status,
                    moderation_reasons: photo.moderation_reasons,
                });
            }
            CreationResult::Created => {}
        }

        let processed = match self.processor.process(&upload).await {
            Ok(photo) => photo,
            Err(error) => {
                self.store.discard_processing(photo_id, user_id).await?;
                return Err(match error {
                    PhotoCodecError::InvalidPhoto(reason) => PhotoError::InvalidPhoto(reason),
                    PhotoCodecError::PhotoTooLarge => PhotoError::PhotoTooLarge,
                    PhotoCodecError::CodecTimedOut | PhotoCodecError::CodecUnavailable => {
                        tracing::warn!(event_code = "photo_codec_unavailable");
                        PhotoError::InvalidPhoto(InvalidPhotoReason::DecodeFailed)
                    }
                });
            }
        };
        let moderation = self.moderation.analyze(&processed.body).await;
        if !self
            .store
            .record_processed(photo_id, user_id, &processed)
            .await?
        {
            self.store.discard_processing(photo_id, user_id).await?;
            return Err(PhotoError::UpdateConflict);
        }
        lease.assert_held()?;
        if self
            .storage
            .put(
                &object_key,
                processed.body,
                processed.mime_type,
                PHOTO_CACHE_CONTROL,
            )
            .await
            .is_err()
        {
            tracing::warn!(event_code = "photo_storage_failed", operation = "upload");
            return Err(PhotoError::StorageUnavailable);
        }
        if !self
            .store
            .activate(photo_id, user_id, moderation.clone())
            .await?
        {
            return Err(PhotoError::UpdateConflict);
        }
        Ok(UploadResult {
            photo: self.sign(&object_key).await?,
            moderation_status: moderation.status,
            moderation_reasons: moderation.reasons,
        })
    }

    pub async fn delete(&self, user_id: Uuid) -> Result<(), PhotoError> {
        if self.store.begin_delete(user_id).await? {
            Ok(())
        } else {
            Err(PhotoError::ProfileNotFound)
        }
    }

    /// Deletes at most one bounded batch while the caller owns the exclusive
    /// account-activity lease used by the resumable erasure workflow.
    pub async fn delete_for_account(
        &self,
        user_id: Uuid,
        batch_size: u32,
    ) -> Result<bool, PhotoError> {
        let photos = self
            .store
            .begin_account_deletion(user_id, batch_size)
            .await?;
        let mut failed = false;
        for photo in &photos {
            if self.storage.delete(&photo.object_key).await.is_err() {
                tracing::warn!(
                    event_code = "photo_storage_failed",
                    operation = "account_deletion"
                );
                failed = true;
                continue;
            }
            if self.store.complete_deletion(photo.id).await.is_err() {
                tracing::warn!(
                    event_code = "photo_storage_failed",
                    operation = "account_deletion"
                );
                failed = true;
            }
        }
        if failed {
            return Err(PhotoError::StorageUnavailable);
        }
        Ok(photos.len() < usize::try_from(batch_size).unwrap_or(usize::MAX))
    }

    pub async fn url_for_object_key(
        &self,
        object_key: Option<String>,
    ) -> Result<Option<String>, PhotoError> {
        let Some(object_key) = object_key else {
            return Ok(None);
        };
        if !valid_profile_photo_key(&object_key) {
            tracing::warn!(event_code = "photo_signing_invalid_key");
            return Ok(None);
        }
        self.sign(&object_key).await.map(Some)
    }

    async fn sign(&self, object_key: &str) -> Result<String, PhotoError> {
        self.storage
            .signed_get_url(object_key, PHOTO_URL_TTL_SECONDS)
            .await
            .map_err(|_| {
                tracing::warn!(event_code = "photo_storage_failed", operation = "sign");
                PhotoError::StorageUnavailable
            })
    }
}

impl ProfilePhotoUrlProvider for PhotoService {
    fn url_for_key(&self, object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async move { self.url_for_object_key(object_key).await.map_err(|_| ()) })
    }
}

pub struct PhotoDeletionHandler {
    store: Arc<dyn PhotoStore>,
    storage: Arc<dyn PhotoObjectStorage>,
}

impl PhotoDeletionHandler {
    pub fn new(store: Arc<dyn PhotoStore>, storage: Arc<dyn PhotoObjectStorage>) -> Self {
        Self { store, storage }
    }
}

impl OutboxHandler for PhotoDeletionHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(async move {
            let photo = self
                .store
                .find_deleting(event.aggregate_id)
                .await
                .map_err(|_| DispatchFailure::transient("photo_delete_failed"))?;
            let Some(photo) = photo else {
                return Ok(DispatchOutcome::Completed);
            };
            self.storage
                .delete(&photo.object_key)
                .await
                .map_err(|_| DispatchFailure::transient("photo_delete_failed"))?;
            self.store
                .complete_deletion(photo.id)
                .await
                .map_err(|_| DispatchFailure::transient("photo_delete_failed"))?;
            Ok(DispatchOutcome::Completed)
        })
    }
}

pub fn upload_request_hash(upload: &UploadPhoto) -> [u8; 32] {
    let mut hash = Sha256::new();
    update_length_prefixed(&mut hash, upload.filename.as_bytes());
    update_length_prefixed(&mut hash, upload.mime_type.trim().to_lowercase().as_bytes());
    update_length_prefixed(&mut hash, &upload.body);
    hash.finalize().into()
}

fn update_length_prefixed(hash: &mut Sha256, value: &[u8]) {
    hash.update(u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    hash.update(value);
}

fn valid_profile_photo_key(value: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(
        r"(?i)^profile-photos/[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}/[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\.webp$",
    ).unwrap_or_else(|_| unreachable!("static photo-key regex is valid"))).is_match(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_filename_normalized_mime_and_body_with_nest_length_prefixes() {
        let upload = UploadPhoto {
            filename: "same.jpg".to_owned(),
            mime_type: " Image/JPEG ".to_owned(),
            body: vec![1, 2, 3],
        };
        assert_eq!(
            upload_request_hash(&upload),
            upload_request_hash(&UploadPhoto {
                mime_type: "image/jpeg".to_owned(),
                ..upload.clone()
            })
        );
        assert_ne!(
            upload_request_hash(&upload),
            upload_request_hash(&UploadPhoto {
                filename: "other.jpg".to_owned(),
                ..upload
            })
        );
    }

    #[test]
    fn only_signs_versioned_profile_photo_keys() {
        let user_id = Uuid::new_v4();
        let photo_id = Uuid::new_v4();
        assert!(valid_profile_photo_key(&format!(
            "profile-photos/{user_id}/{photo_id}.webp"
        )));
        assert!(!valid_profile_photo_key("other/private.webp"));
    }
}
