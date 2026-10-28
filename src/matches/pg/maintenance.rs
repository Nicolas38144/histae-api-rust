//! PostgreSQL transactions and bounded storage operations for match maintenance.

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::Acquire as _;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::infra::postgres_locks::MATCH_MAINTENANCE_LOCK;
use crate::matches::domain::MATCH_PURGE_DAYS;
use crate::matches::maintenance::{MatchMaintenanceBatch, MatchMaintenanceResult};

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
            let batch = match run_batch(&mut transaction, now, batch_size).await {
                Ok(batch) => {
                    transaction.commit().await.map_err(map_sqlx_error)?;
                    batch
                }
                Err(error) => {
                    transaction.rollback().await.map_err(map_sqlx_error)?;
                    return Err(error);
                }
            };
            totals.merge(batch);
            totals.batches = totals.batches.saturating_add(1);
            totals.work_remaining = batch.is_full(batch_size);
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
    let opened = open_continuation_windows(connection, now, limit).await?;
    let expired = expire_continuation_windows(connection, now, purge_after, limit).await?;
    let warned = super::inactivity::enqueue_warnings(connection, now, limit).await?;
    let closed_inactive = super::inactivity::close_inactive_matches(connection, now, limit).await?;
    let deleted_messages = delete_expired_messages(connection, now, limit).await?;
    let detached_reports = detach_expired_reports(connection, now, limit).await?;
    let deleted_notifications = delete_expired_notifications(connection, now, limit).await?;
    let purged = purge_expired_matches(connection, now, limit).await?;
    Ok(MatchMaintenanceBatch {
        opened,
        expired,
        warned,
        closed_inactive,
        deleted_messages,
        deleted_notifications,
        detached_reports,
        purged,
    })
}

async fn open_continuation_windows(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT id FROM match_init
           WHERE status = 'active' AND expires_at <= $1
           ORDER BY expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )
         UPDATE match_init AS match_record
         SET status = 'awaiting_continuation', expires_at = match_record.expires_at + INTERVAL '24 hours'
         FROM candidates WHERE match_record.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}

async fn expire_continuation_windows(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    purge_after: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
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
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}

async fn delete_expired_messages(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
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
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}

async fn detach_expired_reports(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
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
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}

async fn delete_expired_notifications(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT notification.id FROM match_init AS match_record
           JOIN notification
             ON notification.user_id IN (match_record.user1_id, match_record.user2_id)
            AND notification.payload->>'match_id' = match_record.id::text
           WHERE match_record.status IN ('expired', 'ended')
             AND match_record.purge_after <= $1
           ORDER BY match_record.purge_after, match_record.id,
                    notification.created_at, notification.id
           LIMIT $2 FOR UPDATE OF notification SKIP LOCKED
         )
         DELETE FROM notification USING candidates
         WHERE notification.id = candidates.id",
    )
    .bind(now)
    .bind(limit)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}

async fn purge_expired_matches(
    connection: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT match_record.id FROM match_init AS match_record
           WHERE match_record.status IN ('expired', 'ended')
             AND match_record.purge_after <= $1
             AND NOT EXISTS (SELECT 1 FROM chat_message WHERE match_id = match_record.id)
             AND NOT EXISTS (SELECT 1 FROM user_report WHERE match_id = match_record.id)
             AND NOT EXISTS (SELECT 1 FROM notification n
               WHERE n.user_id IN (match_record.user1_id,match_record.user2_id)
                 AND n.payload->>'match_id'=match_record.id::text)
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
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}
