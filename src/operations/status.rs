use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::identity::mobile::otp::{OtpDeliverySnapshot, OtpRepository, OtpStore};
use crate::identity::mobile::sweego::SweegoWebhookMetrics;
use crate::infra::postgres::{Database, DatabaseError, PoolStats, map_sqlx_error};
use crate::operations::maintenance::{MaintenanceSnapshot, PgMaintenanceStatusRepository};

#[derive(Clone, Debug)]
pub struct QueueSnapshot {
    pub pending: i64,
    pub processing: i64,
    pub completed: i64,
    pub dead_letter: i64,
    pub discarded: i64,
    pub oldest_pending_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct PersistentSnapshot {
    pub postgres_pool: PoolStats,
    pub outbox: QueueSnapshot,
    pub notification_push: QueueSnapshot,
    pub billing_reconciliation: QueueSnapshot,
    pub otp: OtpDeliverySnapshot,
    pub webhook_enabled: bool,
    pub callbacks: BTreeMap<&'static str, u64>,
    pub maintenance: Vec<MaintenanceSnapshot>,
}

pub type StatusFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PersistentSnapshot, DatabaseError>> + Send + 'a>>;

pub trait PersistentStatus: Send + Sync {
    fn snapshot(&self) -> StatusFuture<'_>;
}

pub struct PgOperationalStatus {
    database: Database,
    otp: OtpRepository,
    maintenance: PgMaintenanceStatusRepository,
    callbacks: Arc<SweegoWebhookMetrics>,
    webhook_enabled: bool,
}

impl PgOperationalStatus {
    pub fn new(
        database: Database,
        callbacks: Arc<SweegoWebhookMetrics>,
        webhook_enabled: bool,
    ) -> Self {
        Self {
            otp: OtpRepository::new(database.clone()),
            maintenance: PgMaintenanceStatusRepository::new(database.clone()),
            database,
            callbacks,
            webhook_enabled,
        }
    }
}

impl PersistentStatus for PgOperationalStatus {
    fn snapshot(&self) -> StatusFuture<'_> {
        Box::pin(async move {
            type Row = (i64, i64, i64, i64, i64, Option<DateTime<Utc>>);
            let all = sqlx::query_as::<_, Row>(
                "SELECT count(*) FILTER (WHERE status='pending')::bigint,
                        count(*) FILTER (WHERE status='processing')::bigint,
                        count(*) FILTER (WHERE status='completed')::bigint,
                        count(*) FILTER (WHERE status='dead_letter')::bigint,
                        count(*) FILTER (WHERE status='discarded')::bigint,
                        min(available_at) FILTER (WHERE status='pending') FROM outbox_event",
            )
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            let push = sqlx::query_as::<_, Row>(
                "SELECT count(*) FILTER (WHERE status='pending')::bigint,
                        count(*) FILTER (WHERE status='processing')::bigint,
                        count(*) FILTER (WHERE status='completed')::bigint,
                        count(*) FILTER (WHERE status='dead_letter')::bigint,
                        count(*) FILTER (WHERE status='discarded')::bigint,
                        min(available_at) FILTER (WHERE status='pending')
                 FROM outbox_event WHERE event_type='notification.push'",
            )
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            let billing = sqlx::query_as::<_, Row>(
                "SELECT count(*) FILTER (WHERE status='pending')::bigint,
                        count(*) FILTER (WHERE status='processing')::bigint,
                        count(*) FILTER (WHERE status='completed')::bigint,
                        count(*) FILTER (WHERE status='dead_letter')::bigint,
                        count(*) FILTER (WHERE status='discarded')::bigint,
                        min(available_at) FILTER (WHERE status='pending')
                 FROM outbox_event WHERE event_type LIKE 'billing.%'",
            )
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(PersistentSnapshot {
                postgres_pool: self.database.pool_stats(),
                outbox: queue(all),
                notification_push: queue(push),
                billing_reconciliation: queue(billing),
                otp: self.otp.snapshot().await?,
                webhook_enabled: self.webhook_enabled,
                callbacks: self.callbacks.snapshot(),
                maintenance: self.maintenance.list().await?,
            })
        })
    }
}

fn queue(row: (i64, i64, i64, i64, i64, Option<DateTime<Utc>>)) -> QueueSnapshot {
    QueueSnapshot {
        pending: row.0,
        processing: row.1,
        completed: row.2,
        dead_letter: row.3,
        discarded: row.4,
        oldest_pending_at: row.5,
    }
}
