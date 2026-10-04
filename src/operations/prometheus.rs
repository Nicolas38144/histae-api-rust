use std::fmt::Write;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};

use super::metrics::{DURATION_BUCKETS_MS, MetricsSnapshot, OperationalMetrics};
use super::status::{PersistentSnapshot, PersistentStatus, QueueSnapshot};

pub type RenderFuture<'a> = Pin<Box<dyn Future<Output = Result<String, &'static str>> + Send + 'a>>;

pub trait MetricsRenderer: Send + Sync {
    fn render(&self) -> RenderFuture<'_>;
}

pub struct PrometheusExporter {
    metrics: Arc<OperationalMetrics>,
    persistent: Option<Arc<dyn PersistentStatus>>,
    postgres_max: u32,
    otp_ttl: Duration,
}

impl PrometheusExporter {
    pub fn new(metrics: Arc<OperationalMetrics>) -> Self {
        Self {
            metrics,
            persistent: None,
            postgres_max: 0,
            otp_ttl: Duration::ZERO,
        }
    }

    pub fn with_persistent_status(
        metrics: Arc<OperationalMetrics>,
        persistent: Arc<dyn PersistentStatus>,
        postgres_max: u32,
        otp_ttl: Duration,
    ) -> Self {
        Self {
            metrics,
            persistent: Some(persistent),
            postgres_max,
            otp_ttl,
        }
    }
}

impl MetricsRenderer for PrometheusExporter {
    fn render(&self) -> RenderFuture<'_> {
        Box::pin(async move {
            let persistent = match &self.persistent {
                Some(provider) => provider.snapshot().await.ok(),
                None => None,
            };
            let mut output = render_snapshot(
                &self.metrics.snapshot(),
                resident_memory_bytes(),
                persistent.is_some(),
            );
            if let Some(snapshot) = persistent {
                render_persistent(
                    &mut output,
                    &snapshot,
                    self.postgres_max,
                    self.otp_ttl,
                    Utc::now(),
                );
            }
            Ok(output)
        })
    }
}

pub fn render_snapshot(
    snapshot: &MetricsSnapshot,
    rss: Option<u64>,
    collection_succeeded: bool,
) -> String {
    let mut output = String::new();
    gauge(
        &mut output,
        "histae_metrics_collection_success",
        "Whether persistent operational state was collected for this scrape.",
        f64::from(collection_succeeded),
        "",
    );
    gauge(
        &mut output,
        "histae_runtime_info",
        "Runtime identity for this API process.",
        1.0,
        "runtime=\"rust\"",
    );
    gauge(
        &mut output,
        "histae_process_start_time_seconds",
        "Unix timestamp when this API process started.",
        timestamp(snapshot.started_at),
        "",
    );
    gauge(
        &mut output,
        "histae_process_uptime_seconds",
        "API process uptime.",
        snapshot.uptime.as_secs_f64(),
        "",
    );
    if let Some(value) = rss {
        gauge(
            &mut output,
            "histae_process_resident_memory_bytes",
            "Resident memory used by the API process.",
            value as f64,
            "",
        );
    }
    for route in &snapshot.http {
        let labels = format!(
            "method=\"{}\",route=\"{}\"",
            escape(&route.method),
            escape(&route.route)
        );
        counter(
            &mut output,
            "histae_http_requests_total",
            "HTTP requests by normalized route and method.",
            route.requests,
            &labels,
        );
        counter(
            &mut output,
            "histae_http_errors_total",
            "HTTP responses with status 4xx or 5xx.",
            route.errors,
            &labels,
        );
        for (status, value) in [
            ("401", route.status_401),
            ("403", route.status_403),
            ("429", route.status_429),
            ("5xx", route.status_5xx),
        ] {
            counter(
                &mut output,
                "histae_http_responses_total",
                "Security-relevant and server-error HTTP responses.",
                value,
                &format!("{labels},status=\"{status}\""),
            );
        }
        histogram(
            &mut output,
            "histae_http_request_duration_seconds",
            "HTTP request duration.",
            &route.buckets,
            route.total_duration_ms / 1_000.0,
            route.requests,
            &labels,
        );
    }
    for (name, values) in &snapshot.dependencies {
        let labels = format!("dependency=\"{}\"", escape(name));
        gauge(
            &mut output,
            "histae_dependency_enabled",
            "Whether the dependency is configured for this process.",
            1.0,
            &labels,
        );
        let up = match (values.last_success_at, values.last_error_at) {
            (Some(success), Some(error)) => success >= error,
            (Some(_), None) => true,
            _ => false,
        };
        gauge(
            &mut output,
            "histae_dependency_up",
            "Whether the last observed dependency operation succeeded.",
            f64::from(up),
            &labels,
        );
        counter(
            &mut output,
            "histae_dependency_calls_total",
            "Dependency calls by bounded dependency name.",
            values.calls,
            &labels,
        );
        counter(
            &mut output,
            "histae_dependency_errors_total",
            "Dependency call failures by bounded dependency name.",
            values.errors,
            &labels,
        );
        histogram(
            &mut output,
            "histae_dependency_call_duration_seconds",
            "Dependency call duration.",
            &values.buckets,
            values.total_duration_ms / 1_000.0,
            values.calls,
            &labels,
        );
        gauge(
            &mut output,
            "histae_dependency_last_success_timestamp_seconds",
            "Unix timestamp of the last successful dependency operation.",
            values.last_success_at.map(timestamp).unwrap_or(0.0),
            &labels,
        );
        gauge(
            &mut output,
            "histae_dependency_last_error_timestamp_seconds",
            "Unix timestamp of the last failed dependency operation.",
            values.last_error_at.map(timestamp).unwrap_or(0.0),
            &labels,
        );
    }
    output
}

fn render_persistent(
    out: &mut String,
    snapshot: &PersistentSnapshot,
    postgres_max: u32,
    otp_ttl: Duration,
    now: DateTime<Utc>,
) {
    for (state, value) in [
        ("total", snapshot.postgres_pool.total),
        ("idle", snapshot.postgres_pool.idle),
        ("waiting", snapshot.postgres_pool.waiting),
    ] {
        gauge(
            out,
            "histae_postgres_pool_connections",
            "PostgreSQL pool connections by state.",
            f64::from(value),
            &format!("state=\"{state}\""),
        );
    }
    gauge(
        out,
        "histae_postgres_pool_max_connections",
        "Configured PostgreSQL application pool limit.",
        f64::from(postgres_max),
        "",
    );
    for (status, value) in [
        ("pending", snapshot.outbox.pending),
        ("processing", snapshot.outbox.processing),
        ("dead_letter", snapshot.outbox.dead_letter),
        ("discarded", snapshot.outbox.discarded),
    ] {
        gauge(
            out,
            "histae_outbox_events",
            "Current outbox events by status.",
            value as f64,
            &format!("status=\"{status}\""),
        );
    }
    gauge(
        out,
        "histae_outbox_oldest_pending_age_seconds",
        "Age of the oldest pending outbox event.",
        age(snapshot.outbox.oldest_pending_at, now),
        "",
    );
    render_queue(out, "notification_push", &snapshot.notification_push, now);
    render_queue(
        out,
        "billing_reconciliation",
        &snapshot.billing_reconciliation,
        now,
    );
    for (state, value) in [
        ("pending", snapshot.otp.states.pending),
        ("accepted", snapshot.otp.states.accepted),
        ("sent", snapshot.otp.states.sent),
        ("failed", snapshot.otp.states.failed),
        ("unknown", snapshot.otp.states.unknown),
    ] {
        gauge(
            out,
            "histae_sweego_otp_deliveries",
            "Unexpired OTP deliveries by state.",
            f64::from(value),
            &format!("state=\"{state}\""),
        );
    }
    gauge(
        out,
        "histae_sweego_awaiting_callback",
        "Accepted OTP deliveries still awaiting a provider callback.",
        f64::from(snapshot.otp.awaiting_callback),
        "",
    );
    gauge(
        out,
        "histae_sweego_oldest_unresolved_age_seconds",
        "Age of the oldest unresolved OTP delivery.",
        snapshot.otp.oldest_unresolved_age_seconds.unwrap_or(0.0),
        "",
    );
    gauge(
        out,
        "histae_sweego_otp_ttl_seconds",
        "Configured OTP lifetime.",
        otp_ttl.as_secs_f64(),
        "",
    );
    gauge(
        out,
        "histae_sweego_webhook_enabled",
        "Whether authenticated Sweego callbacks are enabled.",
        f64::from(snapshot.webhook_enabled),
        "",
    );
    for (outcome, value) in &snapshot.callbacks {
        counter(
            out,
            "histae_sweego_webhook_callbacks_total",
            "Sweego webhook callbacks by bounded outcome.",
            *value,
            &format!("outcome=\"{}\"", escape(outcome)),
        );
    }
    for job_name in ["matches", "photos", "privacy", "outbox", "billing"] {
        let job = snapshot
            .maintenance
            .iter()
            .find(|entry| entry.job_name.as_str() == job_name);
        let labels = format!("job=\"{job_name}\"");
        gauge(
            out,
            "histae_maintenance_missing",
            "Whether no persistent run exists for the maintenance job.",
            f64::from(job.is_none()),
            &labels,
        );
        let overdue = job.is_none_or(|entry| {
            let interval = if job_name == "privacy" {
                86_400
            } else if job_name == "outbox" {
                60
            } else if job_name == "billing" {
                300
            } else {
                3_600
            };
            let reference = entry.finished_at.unwrap_or(entry.started_at);
            (entry.status.as_str() == "running"
                && now.signed_duration_since(entry.started_at).num_seconds() > interval)
                || now.signed_duration_since(reference).num_seconds() > interval * 2
        });
        gauge(
            out,
            "histae_maintenance_overdue",
            "Whether the maintenance job is overdue or stuck.",
            f64::from(overdue),
            &labels,
        );
        gauge(
            out,
            "histae_maintenance_work_remaining",
            "Whether the last maintenance pass reached its work budget.",
            f64::from(job.is_some_and(|entry| entry.work_remaining)),
            &labels,
        );
        for status in ["running", "succeeded", "failed", "skipped", "missing"] {
            let current = job.map_or("missing", |entry| entry.status.as_str());
            gauge(
                out,
                "histae_maintenance_last_run_status",
                "Last maintenance run status as a one-hot value.",
                f64::from(current == status),
                &format!("{labels},status=\"{status}\""),
            );
        }
        gauge(
            out,
            "histae_maintenance_last_duration_seconds",
            "Duration of the last maintenance pass.",
            job.and_then(|entry| entry.duration_ms)
                .map_or(0.0, |value| f64::from(value) / 1_000.0),
            &labels,
        );
        gauge(
            out,
            "histae_maintenance_last_processed",
            "Items processed by the last maintenance pass.",
            job.map_or(0.0, |entry| entry.processed_count as f64),
            &labels,
        );
        gauge(
            out,
            "histae_maintenance_last_batches",
            "Batches processed by the last maintenance pass.",
            job.map_or(0.0, |entry| f64::from(entry.batch_count)),
            &labels,
        );
        gauge(
            out,
            "histae_maintenance_last_success_timestamp_seconds",
            "Unix timestamp of the last successful maintenance pass.",
            job.and_then(|entry| entry.last_succeeded_at)
                .map_or(0.0, |value| value.timestamp_millis() as f64 / 1_000.0),
            &labels,
        );
    }
}

fn render_queue(out: &mut String, name: &str, queue: &QueueSnapshot, now: DateTime<Utc>) {
    for (status, value) in [
        ("pending", queue.pending),
        ("processing", queue.processing),
        ("completed", queue.completed),
        ("dead_letter", queue.dead_letter),
        ("discarded", queue.discarded),
    ] {
        gauge(
            out,
            "histae_outbox_queue_events",
            "Current outbox events in operational queues.",
            value as f64,
            &format!("queue=\"{name}\",status=\"{status}\""),
        );
    }
    gauge(
        out,
        "histae_outbox_queue_oldest_pending_age_seconds",
        "Age of the oldest pending event in an operational queue.",
        age(queue.oldest_pending_at, now),
        &format!("queue=\"{name}\""),
    );
}

fn age(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f64 {
    value.map_or(0.0, |then| {
        now.signed_duration_since(then).num_milliseconds().max(0) as f64 / 1_000.0
    })
}

fn histogram(
    out: &mut String,
    name: &str,
    help: &str,
    buckets: &[u64; 11],
    sum: f64,
    count: u64,
    labels: &str,
) {
    describe(out, name, help, "histogram");
    let mut cumulative = 0_u64;
    for (index, bound) in DURATION_BUCKETS_MS.iter().enumerate() {
        cumulative = cumulative.saturating_add(buckets[index]);
        let le = if bound.is_infinite() {
            "+Inf".to_owned()
        } else {
            format_number(*bound / 1_000.0)
        };
        sample(
            out,
            &format!("{name}_bucket"),
            cumulative as f64,
            &format_labels(labels, &format!("le=\"{le}\"")),
        );
    }
    sample(out, &format!("{name}_sum"), sum, labels);
    sample(out, &format!("{name}_count"), count as f64, labels);
}
fn counter(out: &mut String, name: &str, help: &str, value: u64, labels: &str) {
    describe(out, name, help, "counter");
    sample(out, name, value as f64, labels);
}
fn gauge(out: &mut String, name: &str, help: &str, value: f64, labels: &str) {
    describe(out, name, help, "gauge");
    sample(out, name, value, labels);
}
fn describe(out: &mut String, name: &str, help: &str, kind: &str) {
    if out.contains(&format!("# HELP {name} ")) {
        return;
    }
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}
fn sample(out: &mut String, name: &str, value: f64, labels: &str) {
    let labels = if labels.is_empty() {
        String::new()
    } else {
        format!("{{{labels}}}")
    };
    let _ = writeln!(out, "{name}{labels} {}", format_number(value));
}
fn format_labels(left: &str, right: &str) -> String {
    if left.is_empty() {
        right.to_owned()
    } else {
        format!("{left},{right}")
    }
}
fn format_number(value: f64) -> String {
    if value.is_finite() {
        value.to_string()
    } else {
        "0".to_owned()
    }
}
fn timestamp(value: SystemTime) -> f64 {
    value
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
fn resident_memory_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_ascii_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kb.checked_mul(1_024)
}
#[cfg(not(target_os = "linux"))]
fn resident_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_business_contract_and_reports_rust_truthfully() {
        let metrics = OperationalMetrics::new();
        metrics.record_http("GET", "/api/users/me", 429, 25.0);
        metrics.record_dependency("postgres", false, std::time::Duration::from_millis(6));
        let output = render_snapshot(&metrics.snapshot(), Some(512), true);
        assert!(output.contains("histae_runtime_info{runtime=\"rust\"} 1"));
        assert!(output.contains(
            "histae_http_responses_total{method=\"GET\",route=\"/api/users/me\",status=\"429\"} 1"
        ));
        assert!(output.contains("le=\"0.025\""));
        assert!(!output.contains("heap_used"));
        assert!(!output.contains("event_loop_delay"));
    }
}
