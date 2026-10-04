use std::sync::Arc;

use chrono::Utc;

use crate::billing::reconcile::{
    BillingReconciliationScheduler, PgBillingReconciliationStore, ReconciliationStoreError,
};
use crate::config::AppConfig;
use crate::infra::postgres::Database;
use crate::matches::maintenance::{MatchMaintenanceRepository, MatchMaintenanceService};
use crate::media::maintenance::{PhotoMaintenanceService, PhotoMaintenanceStore};
use crate::media::pg::PgPhotoRepository;
use crate::media::s3::S3ObjectStorage;
use crate::media::storage::PhotoObjectStorage;
use crate::operations::logging::{self, SafeLogValue};
use crate::operations::maintenance::{MaintenanceTracker, PgMaintenanceStatusRepository};
use crate::outbox::pg::PgOutboxRepository;
use crate::privacy::maintenance::{PrivacyMaintenanceRepository, PrivacyMaintenanceService};

pub async fn run(config: AppConfig) -> Result<(), &'static str> {
    let database = Database::connect(&config.postgres)
        .await
        .map_err(|_| "postgres_connection_failed")?;
    let result = async {
        let tracker = MaintenanceTracker::new(Arc::new(PgMaintenanceStatusRepository::new(
            database.clone(),
        )));
        let storage: Arc<dyn PhotoObjectStorage> = Arc::new(
            S3ObjectStorage::new(&config.object_storage)
                .map_err(|_| "object_storage_invalid_configuration")?,
        );
        let photo_repository = Arc::new(PgPhotoRepository::new(
            database.clone(),
            PgOutboxRepository::new(database.clone()),
        ));
        let photo_maintenance_store: Arc<dyn PhotoMaintenanceStore> = photo_repository;
        let matches = MatchMaintenanceService::new(
            MatchMaintenanceRepository::new(database.clone()),
            tracker.clone(),
            config.workloads.clone(),
        );
        let privacy = PrivacyMaintenanceService::new(
            PrivacyMaintenanceRepository::new(database.clone()),
            tracker.clone(),
        );
        let photos =
            PhotoMaintenanceService::new(photo_maintenance_store, storage, tracker.clone());
        let billing = BillingReconciliationScheduler::new(
            Arc::new(PgBillingReconciliationStore::new(database.clone())),
            tracker,
            config.billing.clone(),
        );
        let now = Utc::now();
        let (match_result, privacy_result, photo_result, billing_result) = tokio::join!(
            matches.run_once(now),
            privacy.run_once(),
            photos.run_once(),
            billing.run_once(now),
        );
        let match_result = match_result.map_err(|_| "match_maintenance_failed")?;
        let privacy_result = privacy_result.map_err(|_| "privacy_maintenance_failed")?;
        let photo_result = photo_result.map_err(|_| "photo_maintenance_failed")?;
        let billing_result = billing_result.map_err(map_billing_error)?;
        let matches_processed = match_result.map_or(0, |result| result.processed());
        let privacy_processed = privacy_result.map_or(0, |run| run.result.processed());
        let billing_processed = u64::from(billing_result.unwrap_or_default());
        logging::info(
            "maintenance_completed",
            &[
                (
                    "matches_processed",
                    SafeLogValue::Unsigned(matches_processed),
                ),
                (
                    "privacy_processed",
                    SafeLogValue::Unsigned(privacy_processed),
                ),
                (
                    "photos_cleaned",
                    SafeLogValue::Unsigned(photo_result.result.cleaned),
                ),
                (
                    "photo_failures",
                    SafeLogValue::Unsigned(photo_result.result.failed),
                ),
                (
                    "photos_expired_requests",
                    SafeLogValue::Unsigned(photo_result.result.expired_idempotency_records),
                ),
                (
                    "billing_processed",
                    SafeLogValue::Unsigned(billing_processed),
                ),
            ],
        )
        .map_err(|_| "maintenance_log_failed")?;
        Ok(())
    }
    .await;
    database.close().await;
    result
}

fn map_billing_error(error: ReconciliationStoreError) -> &'static str {
    match error {
        ReconciliationStoreError::Database(_) => "billing_reconciliation_schedule_failed",
        ReconciliationStoreError::InvalidStoredData => "billing_reconciliation_state_invalid",
    }
}
