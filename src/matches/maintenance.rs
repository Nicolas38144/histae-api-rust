use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::config::WorkloadConfig;
use crate::infra::postgres::DatabaseError;
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};

pub use super::pg::MatchMaintenanceRepository;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MatchMaintenanceBatch {
    pub opened: u64,
    pub expired: u64,
    pub warned: u64,
    pub closed_inactive: u64,
    pub deleted_messages: u64,
    pub deleted_notifications: u64,
    pub detached_reports: u64,
    pub purged: u64,
}

impl MatchMaintenanceBatch {
    pub(super) fn is_full(self, batch_size: u32) -> bool {
        let limit = u64::from(batch_size);
        [
            self.opened,
            self.expired,
            self.warned,
            self.closed_inactive,
            self.deleted_messages,
            self.deleted_notifications,
            self.detached_reports,
            self.purged,
        ]
        .into_iter()
        .any(|count| count >= limit)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MatchMaintenanceResult {
    pub opened: u64,
    pub expired: u64,
    pub warned: u64,
    pub closed_inactive: u64,
    pub deleted_messages: u64,
    pub deleted_notifications: u64,
    pub detached_reports: u64,
    pub purged: u64,
    pub batches: u32,
    pub work_remaining: bool,
}

impl MatchMaintenanceResult {
    pub fn processed(self) -> u64 {
        self.opened
            .saturating_add(self.expired)
            .saturating_add(self.warned)
            .saturating_add(self.closed_inactive)
            .saturating_add(self.deleted_messages)
            .saturating_add(self.deleted_notifications)
            .saturating_add(self.detached_reports)
            .saturating_add(self.purged)
    }

    pub(super) fn merge(&mut self, batch: MatchMaintenanceBatch) {
        self.opened = self.opened.saturating_add(batch.opened);
        self.expired = self.expired.saturating_add(batch.expired);
        self.warned = self.warned.saturating_add(batch.warned);
        self.closed_inactive = self.closed_inactive.saturating_add(batch.closed_inactive);
        self.deleted_messages = self.deleted_messages.saturating_add(batch.deleted_messages);
        self.deleted_notifications = self
            .deleted_notifications
            .saturating_add(batch.deleted_notifications);
        self.detached_reports = self.detached_reports.saturating_add(batch.detached_reports);
        self.purged = self.purged.saturating_add(batch.purged);
    }
}

#[derive(Clone)]
pub struct MatchMaintenanceService {
    repository: MatchMaintenanceRepository,
    tracker: MaintenanceTracker,
    workloads: Arc<WorkloadConfig>,
}

impl MatchMaintenanceService {
    pub fn new(
        repository: MatchMaintenanceRepository,
        tracker: MaintenanceTracker,
        workloads: WorkloadConfig,
    ) -> Self {
        Self {
            repository,
            tracker,
            workloads: Arc::new(workloads),
        }
    }

    pub async fn run_once(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<MatchMaintenanceResult>, DatabaseError> {
        self.tracker
            .track(
                MaintenanceJobName::Matches,
                self.repository.run_as_leader(
                    now,
                    self.workloads.match_maintenance_batch_size,
                    self.workloads.match_maintenance_max_batches,
                ),
                |result| MaintenanceProgress {
                    processed_count: result.processed(),
                    batch_count: result.batches,
                    work_remaining: result.work_remaining,
                },
                |_| "match_maintenance_failed",
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_batch_detection_uses_the_largest_independent_operation() {
        let mut batch = MatchMaintenanceBatch {
            deleted_messages: 500,
            ..MatchMaintenanceBatch::default()
        };
        assert!(batch.is_full(500));
        batch.deleted_messages = 499;
        assert!(!batch.is_full(500));
    }
}
