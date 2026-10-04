use std::sync::Arc;
use std::time::Duration;

use crate::app::{BootstrapError, TaskSupervisor, wait_for_shutdown_signal};
use crate::billing::pg::PgBillingRepository;
use crate::billing::reconcile::{
    BillingReconciliationHandler, BillingReconciliationService, PgBillingReconciliationStore,
};
use crate::billing::service::BillingService;
use crate::billing::stripe::StripeClient;
use crate::config::AppConfig;
use crate::infra::postgres::Database;
use crate::infra::postgres_locks::AccountActivityPool;
use crate::infra::redis::RedisService;
use crate::media::pg::PgPhotoRepository;
use crate::media::s3::S3ObjectStorage;
use crate::media::service::PhotoDeletionHandler;
use crate::media::storage::PhotoObjectStorage;
use crate::media::store::PhotoStore;
use crate::notifications::delivery::{
    MobileDeliveryService, NotificationPushHandler, PgNotificationDeliveryStore,
};
use crate::notifications::push::PushService;
use crate::notifications::sse::RealtimeService;
use crate::operations::logging;
use crate::operations::maintenance::{MaintenanceTracker, PgMaintenanceStatusRepository};
use crate::outbox::pg::PgOutboxRepository;
use crate::outbox::worker::{OutboxEventDispatcher, OutboxWorker, OutboxWorkerConfig};
use crate::privacy::erasure::ErasureService;
use crate::privacy::erasure::pg::PgErasureRepository;
use crate::shared::clock::SystemClock;

const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(config: AppConfig) -> Result<(), &'static str> {
    let database = Database::connect(&config.postgres)
        .await
        .map_err(|_| "postgres_connection_failed")?;
    let activity = AccountActivityPool::connect(&config.postgres)
        .await
        .map_err(|_| "account_activity_unavailable")?;
    let redis = RedisService::connect(&config.redis, true)
        .await
        .map_err(|_| "redis_connection_failed")?;
    let realtime = RealtimeService::connect(redis)
        .await
        .map_err(|_| "redis_subscription_failed")?;
    let storage: Arc<dyn PhotoObjectStorage> = Arc::new(
        S3ObjectStorage::new(&config.object_storage)
            .map_err(|_| "object_storage_invalid_configuration")?,
    );
    let stripe = Arc::new(
        StripeClient::new(config.billing.clone()).map_err(|_| "stripe_invalid_configuration")?,
    );
    let outbox = Arc::new(PgOutboxRepository::new(database.clone()));
    let photos = Arc::new(PgPhotoRepository::new(database.clone(), (*outbox).clone()));
    let photo_store: Arc<dyn PhotoStore> = photos.clone();
    let photo_handler = Arc::new(PhotoDeletionHandler::new(photo_store, Arc::clone(&storage)));
    let push_store = Arc::new(PgNotificationDeliveryStore::new(database.clone()));
    let push_sender = Arc::new(PushService::new(config.push.clone(), push_store.clone()));
    let push_handler = Arc::new(NotificationPushHandler::new(push_store, push_sender));
    let billing_store = Arc::new(PgBillingRepository::new(database.clone()));
    let billing = Arc::new(BillingService::new(
        billing_store,
        stripe.clone(),
        config.billing.clone(),
        activity.clone(),
        Arc::new(SystemClock),
    ));
    let erasure_store = Arc::new(PgErasureRepository::new(database.clone()));
    let erasure = Arc::new(ErasureService::new(
        erasure_store,
        activity.clone(),
        billing,
        photo_handler.clone(),
    ));
    let reconciliation_store = Arc::new(PgBillingReconciliationStore::new(database.clone()));
    let delivery = Arc::new(MobileDeliveryService::new(realtime));
    let reconciliation = BillingReconciliationService::new(
        reconciliation_store,
        stripe,
        delivery,
        Arc::new(activity.clone()),
        Arc::new(SystemClock),
        config.billing.clone(),
    );
    let billing_handler = Arc::new(BillingReconciliationHandler::new(reconciliation));
    let dispatcher = Arc::new(OutboxEventDispatcher::new(
        photo_handler,
        push_handler,
        erasure,
        billing_handler.clone(),
        billing_handler,
    ));
    let tracker = MaintenanceTracker::new(Arc::new(PgMaintenanceStatusRepository::new(
        database.clone(),
    )));
    let mut worker = OutboxWorker::new(
        outbox,
        dispatcher,
        tracker,
        OutboxWorkerConfig::from_workloads(&config.workloads),
    );
    let _ = logging::info("outbox_worker_started", &[]);
    let mut supervisor = TaskSupervisor::new(WORKER_DRAIN_TIMEOUT);
    let cancellation = supervisor.cancellation_token();
    supervisor.spawn(async move {
        worker.run_until_cancelled(cancellation).await;
        Ok(())
    });
    let signal = tokio::select! {
        signal = wait_for_shutdown_signal() => signal,
        worker = supervisor.wait_for_exit() => worker,
    };
    let shutdown = supervisor.shutdown().await;
    activity.close().await;
    database.close().await;
    signal.map_err(BootstrapError::safe_code)?;
    shutdown.map_err(BootstrapError::safe_code)
}
