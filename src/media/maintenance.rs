use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

use super::domain::PhotoObject;
use super::storage::PhotoObjectStorage;
use crate::infra::postgres::DatabaseError;
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};

pub const PHOTO_PROCESSING_STALE_MINUTES: i64 = 30;
pub const PHOTO_DELETION_RETRY_MINUTES: i64 = 5;
pub const PHOTO_MAINTENANCE_BATCH_SIZE: u32 = 100;
pub const PHOTO_MAINTENANCE_MAX_BATCHES: u32 = 100;

pub type PhotoMaintenanceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait PhotoMaintenanceStore: Send + Sync {
    fn purge_expired_upload_requests(
        &self,
        before: DateTime<Utc>,
        limit: u32,
    ) -> PhotoMaintenanceFuture<'_, u64>;

    fn claim_cleanup_batch(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        retry_before: DateTime<Utc>,
        limit: u32,
    ) -> PhotoMaintenanceFuture<'_, Vec<PhotoObject>>;

    fn complete_cleanup(&self, photo_id: Uuid) -> PhotoMaintenanceFuture<'_, ()>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhotoMaintenanceResult {
    pub cleaned: u64,
    pub failed: u64,
    pub expired_idempotency_records: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhotoMaintenanceRun {
    pub result: PhotoMaintenanceResult,
    pub batch_count: u32,
    pub work_remaining: bool,
}

#[derive(Clone)]
pub struct PhotoMaintenanceService {
    store: Arc<dyn PhotoMaintenanceStore>,
    storage: Arc<dyn PhotoObjectStorage>,
    tracker: MaintenanceTracker,
}

impl PhotoMaintenanceService {
    pub fn new(
        store: Arc<dyn PhotoMaintenanceStore>,
        storage: Arc<dyn PhotoObjectStorage>,
        tracker: MaintenanceTracker,
    ) -> Self {
        Self {
            store,
            storage,
            tracker,
        }
    }

    pub async fn run_once(&self) -> Result<PhotoMaintenanceRun, DatabaseError> {
        self.tracker
            .track(
                MaintenanceJobName::Photos,
                async { self.perform().await.map(Some) },
                |run| MaintenanceProgress {
                    processed_count: run
                        .result
                        .cleaned
                        .saturating_add(run.result.failed)
                        .saturating_add(run.result.expired_idempotency_records),
                    batch_count: run.batch_count,
                    work_remaining: run.work_remaining,
                },
                |_| "photo_maintenance_failed",
            )
            .await?
            .ok_or(DatabaseError::QueryFailed)
    }

    async fn perform(&self) -> Result<PhotoMaintenanceRun, DatabaseError> {
        let mut totals = PhotoMaintenanceResult::default();
        for batch in 0..PHOTO_MAINTENANCE_MAX_BATCHES {
            let now = Utc::now();
            let stale_before = now
                .checked_sub_signed(TimeDelta::minutes(PHOTO_PROCESSING_STALE_MINUTES))
                .ok_or(DatabaseError::QueryFailed)?;
            let retry_before = now
                .checked_sub_signed(TimeDelta::minutes(PHOTO_DELETION_RETRY_MINUTES))
                .ok_or(DatabaseError::QueryFailed)?;
            let expired = self
                .store
                .purge_expired_upload_requests(now, PHOTO_MAINTENANCE_BATCH_SIZE)
                .await?;
            totals.expired_idempotency_records =
                totals.expired_idempotency_records.saturating_add(expired);
            let photos = self
                .store
                .claim_cleanup_batch(
                    now,
                    stale_before,
                    retry_before,
                    PHOTO_MAINTENANCE_BATCH_SIZE,
                )
                .await?;
            let photo_count = u64::try_from(photos.len()).unwrap_or(u64::MAX);
            for photo in photos {
                let deleted = self.storage.delete(&photo.object_key).await.is_ok();
                if deleted && self.store.complete_cleanup(photo.id).await.is_ok() {
                    totals.cleaned = totals.cleaned.saturating_add(1);
                } else {
                    totals.failed = totals.failed.saturating_add(1);
                }
            }
            let batch_count = batch.saturating_add(1);
            if photo_count < u64::from(PHOTO_MAINTENANCE_BATCH_SIZE)
                && expired < u64::from(PHOTO_MAINTENANCE_BATCH_SIZE)
            {
                return Ok(PhotoMaintenanceRun {
                    result: totals,
                    batch_count,
                    work_remaining: false,
                });
            }
        }
        Ok(PhotoMaintenanceRun {
            result: totals,
            batch_count: PHOTO_MAINTENANCE_MAX_BATCHES,
            work_remaining: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::media::storage::{ObjectStorageError, StorageFuture};
    use crate::operations::maintenance::{
        MaintenanceFinish, MaintenanceFuture, MaintenanceJobName, MaintenanceStatusStore,
    };
    use crate::profiles::domain::ModerationStatus;

    use super::*;

    struct Store {
        batches: Mutex<VecDeque<Vec<PhotoObject>>>,
        completed: Mutex<Vec<Uuid>>,
    }

    impl PhotoMaintenanceStore for Store {
        fn purge_expired_upload_requests(
            &self,
            _before: DateTime<Utc>,
            _limit: u32,
        ) -> PhotoMaintenanceFuture<'_, u64> {
            Box::pin(async { Ok(2) })
        }

        fn claim_cleanup_batch(
            &self,
            _now: DateTime<Utc>,
            _stale_before: DateTime<Utc>,
            _retry_before: DateTime<Utc>,
            _limit: u32,
        ) -> PhotoMaintenanceFuture<'_, Vec<PhotoObject>> {
            Box::pin(async move {
                self.batches
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .pop_front()
                    .ok_or(DatabaseError::QueryFailed)
            })
        }

        fn complete_cleanup(&self, photo_id: Uuid) -> PhotoMaintenanceFuture<'_, ()> {
            Box::pin(async move {
                self.completed
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(photo_id);
                Ok(())
            })
        }
    }

    struct Storage {
        fail_key: String,
    }

    impl PhotoObjectStorage for Storage {
        fn put<'a>(
            &'a self,
            _key: &'a str,
            _body: Vec<u8>,
            _content_type: &'static str,
            _cache_control: &'static str,
        ) -> StorageFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
            Box::pin(async move {
                if key == self.fail_key {
                    Err(ObjectStorageError)
                } else {
                    Ok(())
                }
            })
        }

        fn signed_get_url<'a>(
            &'a self,
            _key: &'a str,
            _ttl_seconds: u32,
        ) -> StorageFuture<'a, String> {
            Box::pin(async { Err(ObjectStorageError) })
        }

        fn check(&self) -> StorageFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    struct NullStatus;

    impl MaintenanceStatusStore for NullStatus {
        fn start<'a>(
            &'a self,
            _job_name: MaintenanceJobName,
            _run_id: Uuid,
            _started_at: DateTime<Utc>,
        ) -> MaintenanceFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn finish<'a>(&'a self, _finish: MaintenanceFinish) -> MaintenanceFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    fn photo(id: Uuid, object_key: &str) -> PhotoObject {
        PhotoObject {
            id,
            user_id: Uuid::new_v4(),
            object_key: object_key.to_owned(),
            moderation_status: ModerationStatus::Pending,
            moderation_reasons: Vec::new(),
        }
    }

    #[tokio::test]
    async fn cleans_successful_objects_and_leaves_failures_for_a_later_claim() {
        let cleaned_id = Uuid::new_v4();
        let failed_id = Uuid::new_v4();
        let failed_key = format!("generated/{failed_id}.webp");
        let store = Arc::new(Store {
            batches: Mutex::new(VecDeque::from([vec![
                photo(cleaned_id, &format!("generated/{cleaned_id}.webp")),
                photo(failed_id, &failed_key),
            ]])),
            completed: Mutex::new(Vec::new()),
        });
        let service = PhotoMaintenanceService::new(
            store.clone(),
            Arc::new(Storage {
                fail_key: failed_key,
            }),
            MaintenanceTracker::new(Arc::new(NullStatus)),
        );
        let run = service.run_once().await.unwrap_or_else(|_| unreachable!());
        assert_eq!(run.result.cleaned, 1);
        assert_eq!(run.result.failed, 1);
        assert_eq!(run.result.expired_idempotency_records, 2);
        assert_eq!(
            store
                .completed
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .as_slice(),
            [cleaned_id]
        );
    }
}
