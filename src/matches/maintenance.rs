use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::Acquire as _;

use crate::config::WorkloadConfig;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::infra::postgres_locks::MATCH_MAINTENANCE_LOCK;
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};

const MATCH_PURGE_DAYS: i64 = 30;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MatchMaintenanceBatch {
    pub opened: u64,
    pub expired: u64,
    pub deleted_messages: u64,
    pub detached_reports: u64,
    pub purged: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MatchMaintenanceResult {
    pub opened: u64,
    pub expired: u64,
    pub deleted_messages: u64,
    pub detached_reports: u64,
    pub purged: u64,
    pub batches: u32,
    pub work_remaining: bool,
}

impl MatchMaintenanceResult {
    pub fn processed(self) -> u64 {
        self.opened
            .saturating_add(self.expired)
            .saturating_add(self.deleted_messages)
            .saturating_add(self.detached_reports)
            .saturating_add(self.purged)
    }

    fn merge(&mut self, batch: MatchMaintenanceBatch) {
        self.opened = self.opened.saturating_add(batch.opened);
        self.expired = self.expired.saturating_add(batch.expired);
        self.deleted_messages = self.deleted_messages.saturating_add(batch.deleted_messages);
        self.detached_reports = self.detached_reports.saturating_add(batch.detached_reports);
        self.purged = self.purged.saturating_add(batch.purged);
    }
}

#[derive(Clone)]
pub struct MatchMaintenanceRepository {
    database: Database,
}

impl MatchMaintenanceRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn run_as_leader(
        &self,
        now: DateTime<Utc>,
        batch_size: u32,
        max_batches: u32,
    ) -> Result<Option<MatchMaintenanceResult>, DatabaseError> {
        let mut connection = self.database.acquire().await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MATCH_MAINTENANCE_LOCK)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        if !acquired {
            return Ok(None);
        }
        let result = self
            .run_locked(&mut connection, now, batch_size, max_batches)
            .await;
        let unlock = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(MATCH_MAINTENANCE_LOCK)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error);
        match (result, unlock) {
            (Ok(value), Ok(true)) => Ok(Some(value)),
            (Ok(_), Ok(false) | Err(_)) => Err(DatabaseError::QueryFailed),
            (Err(error), _) => Err(error),
        }
    }

    async fn run_locked(
        &self,
        connection: &mut sqlx::PgConnection,
        now: DateTime<Utc>,
        batch_size: u32,
        max_batches: u32,
    ) -> Result<MatchMaintenanceResult, DatabaseError> {
        let mut totals = MatchMaintenanceResult::default();
        for _ in 0..max_batches {
            let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
            let batch = run_batch(&mut transaction, now, batch_size).await;
            match batch {
                Ok(_) => transaction.commit().await.map_err(map_sqlx_error)?,
                Err(error) => {
                    transaction.rollback().await.map_err(map_sqlx_error)?;
                    return Err(error);
                }
            }
            let batch = batch?;
            totals.merge(batch);
            totals.batches = totals.batches.saturating_add(1);
            totals.work_remaining = batch_full(batch, batch_size);
            if !totals.work_remaining {
                break;
            }
        }
        Ok(totals)
    }
}

async fn run_batch(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    batch_size: u32,
) -> Result<MatchMaintenanceBatch, DatabaseError> {
    let limit = i64::from(batch_size);
    let purge_after = now
        .checked_add_signed(TimeDelta::days(MATCH_PURGE_DAYS))
        .ok_or(DatabaseError::QueryFailed)?;
    let opened = sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT id FROM match_init
           WHERE status = 'active' AND expires_at <= $1
           ORDER BY expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )
         UPDATE match_init AS match_record
         SET status = 'awaiting_continuation', expires_at = $1 + INTERVAL '24 hours'
         FROM candidates WHERE match_record.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    let expired = sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT id FROM match_init
           WHERE status = 'awaiting_continuation' AND expires_at <= $1
           ORDER BY expires_at, id LIMIT $3 FOR UPDATE SKIP LOCKED
         )
         UPDATE match_init AS match_record SET status = 'expired', purge_after = $2
         FROM candidates WHERE match_record.id = candidates.id",
    )
    .bind(now)
    .bind(purge_after)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    let deleted_messages = sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT message.id FROM match_init AS match_record
           JOIN chat_message AS message ON message.match_id = match_record.id
           WHERE match_record.status IN ('expired', 'ended')
             AND match_record.purge_after <= $1
           ORDER BY match_record.purge_after, match_record.id,
                    message.created_at, message.id
           LIMIT $2 FOR UPDATE OF message SKIP LOCKED
         )
         DELETE FROM chat_message AS message USING candidates
         WHERE message.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    let detached_reports = sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT report.id FROM match_init AS match_record
           JOIN user_report AS report ON report.match_id = match_record.id
           WHERE match_record.status IN ('expired', 'ended')
             AND match_record.purge_after <= $1
           ORDER BY match_record.purge_after, match_record.id,
                    report.created_at, report.id
           LIMIT $2 FOR UPDATE OF report SKIP LOCKED
         )
         UPDATE user_report AS report SET match_id = NULL
         FROM candidates WHERE report.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    let purged = sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT match_record.id FROM match_init AS match_record
           WHERE match_record.status IN ('expired', 'ended')
             AND match_record.purge_after <= $1
             AND NOT EXISTS (SELECT 1 FROM chat_message WHERE match_id = match_record.id)
             AND NOT EXISTS (SELECT 1 FROM user_report WHERE match_id = match_record.id)
           ORDER BY match_record.purge_after, match_record.id
           LIMIT $2 FOR UPDATE OF match_record SKIP LOCKED
         )
         DELETE FROM match_init AS match_record USING candidates
         WHERE match_record.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .rows_affected();
    Ok(MatchMaintenanceBatch {
        opened,
        expired,
        deleted_messages,
        detached_reports,
        purged,
    })
}

fn batch_full(batch: MatchMaintenanceBatch, batch_size: u32) -> bool {
    let limit = u64::from(batch_size);
    [
        batch.opened,
        batch.expired,
        batch.deleted_messages,
        batch.detached_reports,
        batch.purged,
    ]
    .into_iter()
    .max()
    .is_some_and(|count| count >= limit)
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
        assert!(batch_full(batch, 500));
        batch.deleted_messages = 499;
        assert!(!batch_full(batch, 500));
    }
}
