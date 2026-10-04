use super::domain::{AdminBusinessMetrics, AdminRevenue, RevenuePeriod};
use crate::infra::postgres::DatabaseError;
use std::{future::Future, pin::Pin};

pub type MetricsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait AdminMetricsStore: Send + Sync {
    fn revenue(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminRevenue>;
    fn metrics(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminBusinessMetrics>;
}

pub trait AdminOperationsProvider: Send + Sync {
    fn snapshot(&self) -> MetricsFuture<'_, serde_json::Value>;
}
