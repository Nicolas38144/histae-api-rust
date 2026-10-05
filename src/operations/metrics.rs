use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::http::lifecycle::{HttpObservation, HttpObserver};

pub const DURATION_BUCKETS_MS: [f64; 11] = [
    5.0,
    10.0,
    25.0,
    50.0,
    100.0,
    250.0,
    500.0,
    1_000.0,
    2_500.0,
    5_000.0,
    f64::INFINITY,
];
pub const DEPENDENCIES: [&str; 5] = ["postgres", "redis", "object_storage", "sweego", "stripe"];
const MAX_HTTP_ROUTES: usize = 200;

#[derive(Clone, Debug, Default)]
pub struct Counters {
    pub calls: u64,
    pub errors: u64,
    pub total_duration_ms: f64,
    pub buckets: [u64; 11],
    pub last_success_at: Option<SystemTime>,
    pub last_error_at: Option<SystemTime>,
    pub last_error_code: Option<String>,
    pub last_outcome: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct HttpCounters {
    pub method: String,
    pub route: String,
    pub requests: u64,
    pub errors: u64,
    pub status_401: u64,
    pub status_403: u64,
    pub status_429: u64,
    pub status_5xx: u64,
    pub total_duration_ms: f64,
    pub buckets: [u64; 11],
}

#[derive(Clone, Debug)]
pub struct MetricsSnapshot {
    pub started_at: SystemTime,
    pub uptime: Duration,
    pub http: Vec<HttpCounters>,
    pub dependencies: Vec<(&'static str, Counters)>,
}

struct State {
    routes: BTreeMap<(String, String), HttpCounters>,
    dependencies: BTreeMap<&'static str, Counters>,
}

pub struct OperationalMetrics {
    started_at: SystemTime,
    started: Instant,
    state: Mutex<State>,
}

impl Default for OperationalMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationalMetrics {
    pub fn new() -> Self {
        Self {
            started_at: SystemTime::now(),
            started: Instant::now(),
            state: Mutex::new(State {
                routes: BTreeMap::new(),
                dependencies: DEPENDENCIES
                    .into_iter()
                    .map(|name| (name, Counters::default()))
                    .collect(),
            }),
        }
    }

    pub fn record_http(&self, method: &str, route: &str, status: u16, duration_ms: f64) {
        let normalized = if route.starts_with('/') {
            route
        } else {
            "<unmatched>"
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let key = (method.to_owned(), normalized.to_owned());
        if !state.routes.contains_key(&key) && state.routes.len() >= MAX_HTTP_ROUTES {
            return;
        }
        let counters = state.routes.entry(key).or_insert_with(|| HttpCounters {
            method: method.to_owned(),
            route: normalized.to_owned(),
            requests: 0,
            errors: 0,
            status_401: 0,
            status_403: 0,
            status_429: 0,
            status_5xx: 0,
            total_duration_ms: 0.0,
            buckets: [0; 11],
        });
        counters.requests = counters.requests.saturating_add(1);
        counters.errors = counters.errors.saturating_add(u64::from(status >= 400));
        counters.status_401 = counters.status_401.saturating_add(u64::from(status == 401));
        counters.status_403 = counters.status_403.saturating_add(u64::from(status == 403));
        counters.status_429 = counters.status_429.saturating_add(u64::from(status == 429));
        counters.status_5xx = counters.status_5xx.saturating_add(u64::from(status >= 500));
        counters.total_duration_ms += duration_ms.max(0.0);
        counters.buckets[bucket(duration_ms)] =
            counters.buckets[bucket(duration_ms)].saturating_add(1);
    }

    pub fn record_dependency(&self, name: &'static str, succeeded: bool, duration: Duration) {
        self.record_dependency_with_error(name, succeeded, duration, None);
    }

    pub fn record_dependency_with_error(
        &self,
        name: &'static str,
        succeeded: bool,
        duration: Duration,
        error_code: Option<&str>,
    ) {
        if !DEPENDENCIES.contains(&name) {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(counters) = state.dependencies.get_mut(name) else {
            return;
        };
        counters.calls = counters.calls.saturating_add(1);
        let elapsed = duration.as_secs_f64() * 1_000.0;
        counters.total_duration_ms += elapsed;
        counters.buckets[bucket(elapsed)] = counters.buckets[bucket(elapsed)].saturating_add(1);
        if succeeded {
            counters.last_success_at = Some(SystemTime::now());
            counters.last_outcome = Some(true);
        } else {
            counters.errors = counters.errors.saturating_add(1);
            counters.last_error_at = Some(SystemTime::now());
            counters.last_error_code = Some(normalized_error_code(error_code));
            counters.last_outcome = Some(false);
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let Ok(state) = self.state.lock() else {
            return MetricsSnapshot {
                started_at: self.started_at,
                uptime: self.started.elapsed(),
                http: Vec::new(),
                dependencies: Vec::new(),
            };
        };
        MetricsSnapshot {
            started_at: self.started_at,
            uptime: self.started.elapsed(),
            http: state.routes.values().cloned().collect(),
            dependencies: DEPENDENCIES
                .into_iter()
                .filter_map(|name| {
                    state
                        .dependencies
                        .get(name)
                        .cloned()
                        .map(|value| (name, value))
                })
                .collect(),
        }
    }
}

fn normalized_error_code(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "operation_failed".to_owned();
    };
    let mut characters = value.chars();
    let valid = value.len() <= 64
        && characters
            .next()
            .is_some_and(|value| value.is_ascii_lowercase())
        && characters
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '_');
    if valid {
        value.to_owned()
    } else {
        "operation_failed".to_owned()
    }
}

impl HttpObserver for OperationalMetrics {
    fn record(&self, observation: HttpObservation) {
        self.record_http(
            &observation.method,
            &observation.route,
            observation.status.as_u16(),
            observation.duration_ms,
        );
    }
}

fn bucket(duration_ms: f64) -> usize {
    DURATION_BUCKETS_MS
        .iter()
        .position(|bound| duration_ms.max(0.0) <= *bound)
        .unwrap_or(DURATION_BUCKETS_MS.len() - 1)
}

/// Optional, instance-scoped instrumentation: no payloads, keys or SQL text.
#[derive(Clone, Default)]
pub struct DependencyMetrics(Option<std::sync::Arc<OperationalMetrics>>);
impl DependencyMetrics {
    pub fn new(metrics: std::sync::Arc<OperationalMetrics>) -> Self {
        Self(Some(metrics))
    }
    pub async fn observe<T, E>(
        &self,
        name: &'static str,
        error_code: &'static str,
        operation: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let started = Instant::now();
        let result = operation.await;
        if let Some(metrics) = &self.0 {
            metrics.record_dependency_with_error(
                name,
                result.is_ok(),
                started.elapsed(),
                result.as_ref().err().map(|_| error_code),
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_route_cardinality_and_normalizes_unmatched_routes() {
        let metrics = OperationalMetrics::new();
        metrics.record_http("GET", "unmatched", 404, 6.0);
        for index in 0..250 {
            metrics.record_http("GET", &format!("/route/{index}"), 200, 1.0);
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.http.len(), MAX_HTTP_ROUTES);
        assert!(
            snapshot
                .http
                .iter()
                .any(|route| route.route == "<unmatched>")
        );
    }

    #[tokio::test]
    async fn dependency_observation_preserves_results_and_isolates_instances() {
        let metrics = std::sync::Arc::new(OperationalMetrics::new());
        let other = OperationalMetrics::new();
        let observer = DependencyMetrics::new(metrics.clone());
        assert_eq!(
            observer
                .observe("redis", "redis_command_failed", async { Ok::<_, ()>(42) })
                .await,
            Ok(42)
        );
        assert_eq!(
            observer
                .observe("redis", "redis_command_failed", async { Err::<(), _>(7) })
                .await,
            Err(7)
        );
        let snapshot = metrics.snapshot();
        let (_, counters) = snapshot
            .dependencies
            .iter()
            .find(|(name, _)| *name == "redis")
            .expect("dependency");
        assert_eq!((counters.calls, counters.errors), (2, 1));
        assert_eq!(
            counters.last_error_code.as_deref(),
            Some("redis_command_failed")
        );
        assert!(
            other
                .snapshot()
                .dependencies
                .iter()
                .all(|(_, counters)| counters.calls == 0)
        );
    }

    #[test]
    fn retains_security_statuses_and_nest_histogram_buckets() {
        let metrics = OperationalMetrics::new();
        metrics.record_http("POST", "/api/auth", 401, 10.0);
        let route = metrics.snapshot().http.pop().expect("test route");
        assert_eq!((route.requests, route.errors, route.status_401), (1, 1, 1));
        assert_eq!(route.buckets[1], 1);
    }
}
