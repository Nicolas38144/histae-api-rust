use crate::config::{AppConfig, RateLimitStore, SmsProvider};
use crate::identity::mobile::sweego::SweegoWebhookMetrics;
use crate::infra::{postgres::Database, postgres_locks::AccountActivityPool, redis::RedisService};
use crate::media::s3::S3ObjectStorage;
use crate::notifications::sse::RealtimeService;
use crate::operations::{
    metrics::OperationalMetrics,
    prometheus::{MetricsRenderer, PrometheusExporter},
    status::{PersistentStatus, PgOperationalStatus},
};
use std::{sync::Arc, time::Duration};

pub(super) struct ApiResources {
    pub(super) database: Database,
    pub(super) activity: AccountActivityPool,
    pub(super) redis: RedisService,
    pub(super) storage: Arc<S3ObjectStorage>,
    pub(super) metrics: Arc<OperationalMetrics>,
    pub(super) persistent_status: Arc<PgOperationalStatus>,
    pub(super) callbacks: Arc<SweegoWebhookMetrics>,
    pub(super) realtime: RealtimeService,
}

impl ApiResources {
    pub(super) async fn connect(config: &AppConfig) -> Result<Self, &'static str> {
        let metrics = Arc::new(OperationalMetrics::new());
        let database = Database::connect(&config.postgres)
            .await
            .map_err(|_| "postgres_connection_failed")?
            .with_metrics(Arc::clone(&metrics));
        let activity = AccountActivityPool::connect(&config.postgres)
            .await
            .map_err(|_| "account_activity_unavailable")?;
        let redis_enabled = config.rate_limit.store == RateLimitStore::Redis;
        let redis = RedisService::connect(&config.redis, redis_enabled)
            .await
            .map_err(|_| "redis_connection_failed")?
            .with_metrics(Arc::clone(&metrics));
        let realtime = RealtimeService::connect(redis.clone())
            .await
            .map_err(|_| "redis_subscription_failed")?;
        let storage = Arc::new(
            S3ObjectStorage::new(&config.object_storage)
                .map_err(|_| "object_storage_invalid_configuration")?
                .with_metrics(Arc::clone(&metrics)),
        );
        let callbacks = Arc::new(SweegoWebhookMetrics::default());
        let persistent_status = Arc::new(PgOperationalStatus::new(
            database.clone(),
            Arc::clone(&callbacks),
            config.sms.provider == SmsProvider::Sweego
                && !config.sms.webhook_secret.expose_secret().is_empty(),
        ));
        Ok(Self {
            database,
            activity,
            redis,
            storage,
            metrics,
            persistent_status,
            callbacks,
            realtime,
        })
    }

    pub(super) fn metrics_renderer(&self, otp_ttl: Duration) -> Arc<dyn MetricsRenderer> {
        let persistent: Arc<dyn PersistentStatus> = self.persistent_status.clone();
        Arc::new(PrometheusExporter::with_persistent_status(
            Arc::clone(&self.metrics),
            persistent,
            self.database.pool_stats().max,
            otp_ttl,
        ))
    }

    pub(super) async fn close(self) {
        let Self {
            database,
            activity,
            redis,
            storage,
            metrics,
            persistent_status,
            callbacks,
            realtime,
        } = self;
        drop((
            redis,
            storage,
            metrics,
            persistent_status,
            callbacks,
            realtime,
        ));
        activity.close().await;
        database.close().await;
    }
}
