use super::{
    domain::{AdminMetrics, AdminRevenue, RevenuePeriod},
    store::{AdminMetricsStore, AdminOperationsProvider},
};
use crate::infra::postgres::DatabaseError;
use std::sync::Arc;

#[derive(Clone)]
pub struct AdminMetricsService {
    store: Arc<dyn AdminMetricsStore>,
    operations: Arc<dyn AdminOperationsProvider>,
}

impl AdminMetricsService {
    pub fn new(
        store: Arc<dyn AdminMetricsStore>,
        operations: Arc<dyn AdminOperationsProvider>,
    ) -> Self {
        Self { store, operations }
    }
    pub async fn revenue(&self, period: RevenuePeriod) -> Result<AdminRevenue, DatabaseError> {
        self.store.revenue(period).await
    }
    pub async fn metrics(&self, period: RevenuePeriod) -> Result<AdminMetrics, DatabaseError> {
        let (business, operations) =
            tokio::try_join!(self.store.metrics(period), self.operations.snapshot())?;
        Ok(AdminMetrics {
            business,
            operations,
        })
    }
}
