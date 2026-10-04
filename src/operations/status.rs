use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use crate::administration::metrics::{AdminOperationsProvider, MetricsFuture};
use crate::identity::mobile::otp::{OtpDeliverySnapshot, OtpRepository, OtpStore};
use crate::identity::mobile::sweego::SweegoWebhookMetrics;
use crate::infra::postgres::{Database, DatabaseError, PoolStats, map_sqlx_error};
use crate::operations::maintenance::{MaintenanceSnapshot, PgMaintenanceStatusRepository};
use crate::operations::metrics::{DURATION_BUCKETS_MS, OperationalMetrics};
use crate::operations::prometheus::resident_memory_bytes;

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

#[derive(Clone, Copy, Debug)]
pub struct DependencyConfiguration {
    pub redis: bool,
    pub sweego: bool,
    pub stripe: bool,
}

#[derive(Clone)]
pub struct OperationalStatusView {
    metrics: Arc<OperationalMetrics>,
    persistent: Arc<dyn PersistentStatus>,
    dependencies: DependencyConfiguration,
    billing_interval: Duration,
}

impl OperationalStatusView {
    pub fn new(
        metrics: Arc<OperationalMetrics>,
        persistent: Arc<dyn PersistentStatus>,
        dependencies: DependencyConfiguration,
        billing_interval: Duration,
    ) -> Self {
        Self {
            metrics,
            persistent,
            dependencies,
            billing_interval,
        }
    }

    async fn snapshot_impl(&self) -> Result<Value, DatabaseError> {
        let now = Utc::now();
        let metrics = self.metrics.snapshot();
        let persistent = self.persistent.snapshot().await?;

        let mut routes = metrics
            .http
            .iter()
            .map(|route| {
                json!({
                    "method": route.method,
                    "route": route.route,
                    "requests": route.requests,
                    "errors": route.errors,
                    "status_401": route.status_401,
                    "status_403": route.status_403,
                    "status_429": route.status_429,
                    "status_5xx": route.status_5xx,
                    "average_duration_ms": average(route.total_duration_ms, route.requests),
                    "p95_duration_ms": percentile_upper_bound(&route.buckets, route.requests),
                })
            })
            .collect::<Vec<_>>();
        routes.sort_by(|left, right| {
            right["requests"]
                .as_u64()
                .cmp(&left["requests"].as_u64())
                .then_with(|| left["route"].as_str().cmp(&right["route"].as_str()))
        });

        let http_totals = metrics.http.iter().fold([0_u64; 6], |mut total, route| {
            total[0] = total[0].saturating_add(route.requests);
            total[1] = total[1].saturating_add(route.errors);
            total[2] = total[2].saturating_add(route.status_401);
            total[3] = total[3].saturating_add(route.status_403);
            total[4] = total[4].saturating_add(route.status_429);
            total[5] = total[5].saturating_add(route.status_5xx);
            total
        });

        let dependencies = metrics
            .dependencies
            .iter()
            .map(|(name, counters)| {
                let enabled = match *name {
                    "redis" => self.dependencies.redis,
                    "sweego" => self.dependencies.sweego,
                    "stripe" => self.dependencies.stripe,
                    _ => true,
                };
                let status = if !enabled {
                    "disabled"
                } else {
                    match counters.last_outcome {
                        None => "unknown",
                        Some(true) => "ok",
                        Some(false) => "error",
                    }
                };
                (
                    (*name).to_owned(),
                    json!({
                        "enabled": enabled,
                        "status": status,
                        "calls": counters.calls,
                        "errors": counters.errors,
                        "average_duration_ms": average(counters.total_duration_ms, counters.calls),
                        "last_success_at": counters.last_success_at.map(system_time),
                        "last_error_at": counters.last_error_at.map(system_time),
                        "last_error_code": counters.last_error_code,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();

        let maintenance = ["matches", "photos", "privacy", "outbox", "billing"]
            .into_iter()
            .map(|name| maintenance_view(name, &persistent.maintenance, now, self.billing_interval))
            .collect::<Vec<_>>();

        Ok(json!({
            "collected_at": date_time(now),
            "since": system_time(metrics.started_at),
            "runtime": {
                "runtime": "rust",
                "uptime_seconds": metrics.uptime.as_secs(),
                "memory_rss_bytes": resident_memory_bytes().unwrap_or(0),
            },
            "http": {
                "requests": http_totals[0],
                "errors": http_totals[1],
                "status_401": http_totals[2],
                "status_403": http_totals[3],
                "status_429": http_totals[4],
                "status_5xx": http_totals[5],
                "routes": routes,
            },
            "dependencies": dependencies,
            "postgres_pool": {
                "total": persistent.postgres_pool.total,
                "idle": persistent.postgres_pool.idle,
                "waiting": persistent.postgres_pool.waiting,
            },
            "outbox": {
                "pending": persistent.outbox.pending,
                "processing": persistent.outbox.processing,
                "dead_letter": persistent.outbox.dead_letter,
                "discarded": persistent.outbox.discarded,
                "oldest_pending_at": persistent.outbox.oldest_pending_at.map(date_time),
                "notification_push": queue_value(&persistent.notification_push),
                "billing_reconciliation": queue_value(&persistent.billing_reconciliation),
            },
            "sms_delivery": {
                "states": {
                    "pending": persistent.otp.states.pending,
                    "accepted": persistent.otp.states.accepted,
                    "sent": persistent.otp.states.sent,
                    "failed": persistent.otp.states.failed,
                    "unknown": persistent.otp.states.unknown,
                },
                "awaiting_callback": persistent.otp.awaiting_callback,
                "oldest_unresolved_age_seconds": persistent.otp.oldest_unresolved_age_seconds,
                "average_acceptance_ms": persistent.otp.average_acceptance_ms,
                "average_sent_callback_ms": persistent.otp.average_sent_callback_ms,
                "average_failure_ms": persistent.otp.average_failure_ms,
                "retention": persistent.otp.retention,
                "handset_delivery": persistent.otp.handset_delivery,
                "webhook_enabled": persistent.webhook_enabled,
                "callbacks": persistent.callbacks,
            },
            "maintenance": maintenance,
        }))
    }
}

impl AdminOperationsProvider for OperationalStatusView {
    fn snapshot(&self) -> MetricsFuture<'_, Value> {
        Box::pin(self.snapshot_impl())
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

fn queue_value(queue: &QueueSnapshot) -> Value {
    json!({
        "pending": queue.pending,
        "processing": queue.processing,
        "completed": queue.completed,
        "dead_letter": queue.dead_letter,
        "discarded": queue.discarded,
        "oldest_pending_at": queue.oldest_pending_at.map(date_time),
    })
}

fn maintenance_view(
    name: &str,
    snapshots: &[MaintenanceSnapshot],
    now: DateTime<Utc>,
    billing_interval: Duration,
) -> Value {
    let snapshot = snapshots
        .iter()
        .find(|entry| entry.job_name.as_str() == name);
    let expected = match name {
        "privacy" => Duration::from_secs(86_400),
        "outbox" => Duration::from_secs(60),
        "billing" => billing_interval,
        _ => Duration::from_secs(3_600),
    };
    let Some(snapshot) = snapshot else {
        return json!({
            "job_name": name,
            "status": null,
            "started_at": null,
            "finished_at": null,
            "last_succeeded_at": null,
            "duration_ms": null,
            "processed_count": 0,
            "batch_count": 0,
            "work_remaining": false,
            "last_error_code": null,
            "missing": true,
            "overdue": true,
        });
    };
    let reference = snapshot.finished_at.unwrap_or(snapshot.started_at);
    let elapsed = now
        .signed_duration_since(reference)
        .num_milliseconds()
        .max(0) as u128;
    let running_elapsed = now
        .signed_duration_since(snapshot.started_at)
        .num_milliseconds()
        .max(0) as u128;
    let overdue = (snapshot.status.as_str() == "running" && running_elapsed > expected.as_millis())
        || elapsed > expected.as_millis().saturating_mul(2);
    json!({
        "job_name": name,
        "status": snapshot.status.as_str(),
        "started_at": date_time(snapshot.started_at),
        "finished_at": snapshot.finished_at.map(date_time),
        "last_succeeded_at": snapshot.last_succeeded_at.map(date_time),
        "duration_ms": snapshot.duration_ms,
        "processed_count": snapshot.processed_count,
        "batch_count": snapshot.batch_count,
        "work_remaining": snapshot.work_remaining,
        "last_error_code": snapshot.last_error_code,
        "missing": false,
        "overdue": overdue,
    })
}

fn average(total: f64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        ((total / count as f64) * 10.0).round() / 10.0
    }
}

fn percentile_upper_bound(buckets: &[u64; 11], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let threshold = (total as f64 * 0.95).ceil() as u64;
    let mut cumulative = 0_u64;
    for (index, count) in buckets.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
        if cumulative >= threshold {
            let bound = DURATION_BUCKETS_MS[index];
            return if bound.is_finite() {
                bound
            } else {
                DURATION_BUCKETS_MS[DURATION_BUCKETS_MS.len() - 2]
            };
        }
    }
    DURATION_BUCKETS_MS[DURATION_BUCKETS_MS.len() - 2]
}

fn system_time(value: SystemTime) -> String {
    date_time(DateTime::<Utc>::from(value))
}

fn date_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::mobile::otp::OtpDeliveryStates;

    #[derive(Clone)]
    struct FakePersistentStatus {
        snapshot: PersistentSnapshot,
    }

    impl PersistentStatus for FakePersistentStatus {
        fn snapshot(&self) -> StatusFuture<'_> {
            let snapshot = self.snapshot.clone();
            Box::pin(async move { Ok(snapshot) })
        }
    }

    fn empty_queue() -> QueueSnapshot {
        QueueSnapshot {
            pending: 0,
            processing: 0,
            completed: 0,
            dead_letter: 0,
            discarded: 0,
            oldest_pending_at: None,
        }
    }

    #[tokio::test]
    async fn admin_snapshot_preserves_operational_shape_without_node_metrics() {
        let metrics = Arc::new(OperationalMetrics::new());
        metrics.record_http("GET", "/api/admin/metrics", 403, 26.0);
        metrics.record_dependency_with_error(
            "redis",
            false,
            Duration::from_millis(7),
            Some("connection_refused"),
        );
        let persistent = Arc::new(FakePersistentStatus {
            snapshot: PersistentSnapshot {
                postgres_pool: PoolStats {
                    total: 4,
                    idle: 3,
                    waiting: 1,
                    max: 10,
                },
                outbox: empty_queue(),
                notification_push: empty_queue(),
                billing_reconciliation: empty_queue(),
                otp: OtpDeliverySnapshot {
                    states: OtpDeliveryStates {
                        pending: 0,
                        accepted: 0,
                        sent: 0,
                        failed: 0,
                        unknown: 0,
                    },
                    awaiting_callback: 0,
                    oldest_unresolved_age_seconds: None,
                    average_acceptance_ms: None,
                    average_sent_callback_ms: None,
                    average_failure_ms: None,
                    retention: "otp_expiry",
                    handset_delivery: "not_confirmed",
                },
                webhook_enabled: false,
                callbacks: BTreeMap::new(),
                maintenance: Vec::new(),
            },
        });
        let view = OperationalStatusView::new(
            metrics,
            persistent,
            DependencyConfiguration {
                redis: false,
                sweego: false,
                stripe: false,
            },
            Duration::from_secs(300),
        );

        let snapshot = view.snapshot().await.expect("snapshot");
        assert_eq!(snapshot["runtime"]["runtime"], "rust");
        assert!(snapshot["runtime"].get("heap_used_bytes").is_none());
        assert_eq!(snapshot["dependencies"]["redis"]["status"], "disabled");
        assert_eq!(
            snapshot["dependencies"]["redis"]["last_error_code"],
            "connection_refused"
        );
        assert!(snapshot["runtime"].get("event_loop_delay_p95_ms").is_none());
        assert_eq!(snapshot["http"]["requests"], 1);
        assert_eq!(snapshot["http"]["routes"][0]["p95_duration_ms"], 50.0);
        assert_eq!(snapshot["dependencies"]["redis"]["status"], "disabled");
        assert_eq!(snapshot["postgres_pool"]["waiting"], 1);
        assert_eq!(snapshot["maintenance"].as_array().map(Vec::len), Some(5));
        assert_eq!(snapshot["maintenance"][0]["missing"], true);
    }
}
