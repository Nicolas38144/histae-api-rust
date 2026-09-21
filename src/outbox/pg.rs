use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::outbox::types::{
    ClaimWindow, NewOutboxEvent, OutboxEvent, OutboxEventType, OutboxStatus, RetryResult,
};

pub type OutboxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait OutboxStore: Send + Sync {
    fn claim_batch<'a>(
        &'a self,
        worker_id: Uuid,
        window: ClaimWindow,
        limit: u32,
    ) -> OutboxFuture<'a, Vec<OutboxEvent>>;

    fn renew_claim<'a>(&'a self, event_id: Uuid, worker_id: Uuid) -> OutboxFuture<'a, bool>;

    fn complete<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        processed_at: DateTime<Utc>,
    ) -> OutboxFuture<'a, bool>;

    fn reschedule<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        available_at: DateTime<Utc>,
        error_code: &'static str,
        max_attempts: u16,
    ) -> OutboxFuture<'a, RetryResult>;

    fn purge_resolved<'a>(&'a self, before: DateTime<Utc>, limit: u32) -> OutboxFuture<'a, u64>;
}

#[derive(Clone)]
pub struct PgOutboxRepository {
    database: Database,
}

impl PgOutboxRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn enqueue(
        &self,
        connection: &mut PgConnection,
        event: &NewOutboxEvent,
    ) -> Result<bool, DatabaseError> {
        let result = sqlx::query(
            "INSERT INTO outbox_event (id, event_type, aggregate_id, payload)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (event_type, aggregate_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(event.event_type.as_str())
        .bind(event.aggregate_id)
        .bind(Json(Value::Object(event.payload.clone())))
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn requeue(
        &self,
        connection: &mut PgConnection,
        event: &NewOutboxEvent,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "INSERT INTO outbox_event (id, event_type, aggregate_id, payload)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (event_type, aggregate_id) DO UPDATE
             SET status = 'pending', attempts = 0,
                 available_at = clock_timestamp(), locked_at = NULL, locked_by = NULL,
                 last_error_code = NULL, processed_at = NULL, dead_lettered_at = NULL,
                 resolved_at = NULL, resolved_by = NULL, resolution_reason = NULL",
        )
        .bind(Uuid::new_v4())
        .bind(event.event_type.as_str())
        .bind(event.aggregate_id)
        .bind(Json(Value::Object(event.payload.clone())))
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn claim_batch_query(
        &self,
        worker_id: Uuid,
        window: ClaimWindow,
        limit: u32,
    ) -> Result<Vec<OutboxEvent>, DatabaseError> {
        type ClaimedRow = (Uuid, String, Uuid, Json<Value>, String, i16);
        let rows = sqlx::query_as::<_, ClaimedRow>(
            "WITH candidates AS (
                 SELECT id
                 FROM outbox_event
                 WHERE (status = 'pending' AND available_at <= $2)
                    OR (status = 'processing' AND locked_at <= $3)
                 ORDER BY available_at, created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT $4
             )
             UPDATE outbox_event AS event
             SET status = 'processing', attempts = event.attempts + 1,
                 locked_at = $2, locked_by = $1
             FROM candidates
             WHERE event.id = candidates.id
             RETURNING event.id, event.event_type, event.aggregate_id, event.payload,
                       event.status, event.attempts",
        )
        .bind(worker_id)
        .bind(window.now)
        .bind(window.stale_before)
        .bind(i64::from(limit))
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;

        rows.into_iter().map(map_claimed_row).collect()
    }
}

impl OutboxStore for PgOutboxRepository {
    fn claim_batch<'a>(
        &'a self,
        worker_id: Uuid,
        window: ClaimWindow,
        limit: u32,
    ) -> OutboxFuture<'a, Vec<OutboxEvent>> {
        Box::pin(self.claim_batch_query(worker_id, window, limit))
    }

    fn renew_claim<'a>(&'a self, event_id: Uuid, worker_id: Uuid) -> OutboxFuture<'a, bool> {
        Box::pin(async move {
            let result = sqlx::query(
                "UPDATE outbox_event SET locked_at = clock_timestamp()
                 WHERE id = $1 AND status = 'processing' AND locked_by = $2",
            )
            .bind(event_id)
            .bind(worker_id)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(result.rows_affected() == 1)
        })
    }

    fn complete<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        processed_at: DateTime<Utc>,
    ) -> OutboxFuture<'a, bool> {
        Box::pin(async move {
            let result = sqlx::query(
                "UPDATE outbox_event
                 SET status = 'completed', processed_at = $3,
                     locked_at = NULL, locked_by = NULL, last_error_code = NULL,
                     dead_lettered_at = NULL
                 WHERE id = $1 AND status = 'processing' AND locked_by = $2",
            )
            .bind(event_id)
            .bind(worker_id)
            .bind(processed_at)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(result.rows_affected() == 1)
        })
    }

    fn reschedule<'a>(
        &'a self,
        event_id: Uuid,
        worker_id: Uuid,
        available_at: DateTime<Utc>,
        error_code: &'static str,
        max_attempts: u16,
    ) -> OutboxFuture<'a, RetryResult> {
        Box::pin(async move {
            let max_attempts =
                i16::try_from(max_attempts).map_err(|_| DatabaseError::QueryFailed)?;
            let status = sqlx::query_scalar::<_, String>(
                "UPDATE outbox_event
                 SET status = CASE WHEN attempts >= $5 THEN 'dead_letter' ELSE 'pending' END,
                     available_at = $3, locked_at = NULL, locked_by = NULL,
                     last_error_code = $4,
                     dead_lettered_at = CASE WHEN attempts >= $5 THEN clock_timestamp() ELSE NULL END
                 WHERE id = $1 AND status = 'processing' AND locked_by = $2
                 RETURNING status",
            )
            .bind(event_id)
            .bind(worker_id)
            .bind(available_at)
            .bind(error_code)
            .bind(max_attempts)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            match status.as_deref() {
                Some("pending") => Ok(RetryResult::Pending),
                Some("dead_letter") => Ok(RetryResult::DeadLetter),
                None => Ok(RetryResult::NotOwned),
                Some(_) => Err(DatabaseError::QueryFailed),
            }
        })
    }

    fn purge_resolved<'a>(&'a self, before: DateTime<Utc>, limit: u32) -> OutboxFuture<'a, u64> {
        Box::pin(async move {
            let result = sqlx::query(
                "DELETE FROM outbox_event
                 WHERE id IN (
                     SELECT id FROM (
                         (SELECT id, processed_at AS cleanup_at
                          FROM outbox_event
                          WHERE status = 'completed' AND processed_at <= $1
                          ORDER BY processed_at, id
                          LIMIT $2)
                         UNION ALL
                         (SELECT id, resolved_at AS cleanup_at
                          FROM outbox_event
                          WHERE status = 'discarded' AND resolved_at <= $1
                          ORDER BY resolved_at, id
                          LIMIT $2)
                     ) AS candidates
                     ORDER BY cleanup_at, id
                     LIMIT $2
                 )",
            )
            .bind(before)
            .bind(i64::from(limit))
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(result.rows_affected())
        })
    }
}

fn map_claimed_row(
    row: (Uuid, String, Uuid, Json<Value>, String, i16),
) -> Result<OutboxEvent, DatabaseError> {
    let (id, event_type, aggregate_id, Json(payload), status, attempts) = row;
    let payload = payload
        .as_object()
        .cloned()
        .ok_or(DatabaseError::QueryFailed)?;
    let status = OutboxStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?;
    let attempts = u16::try_from(attempts).map_err(|_| DatabaseError::QueryFailed)?;
    Ok(OutboxEvent {
        id,
        event_type: OutboxEventType::parse(event_type),
        aggregate_id,
        payload,
        status,
        attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_claimed_rows_without_assuming_that_the_database_enum_is_closed() {
        let id = Uuid::new_v4();
        let aggregate_id = Uuid::new_v4();
        let event = map_claimed_row((
            id,
            "future.effect".to_owned(),
            aggregate_id,
            Json(serde_json::json!({})),
            "processing".to_owned(),
            1,
        ))
        .expect("a schema-valid row should map");
        assert_eq!(event.id, id);
        assert_eq!(event.aggregate_id, aggregate_id);
        assert_eq!(
            event.event_type,
            OutboxEventType::Unsupported("future.effect".to_owned())
        );
    }

    #[test]
    fn rejects_rows_that_violate_the_persisted_contract() {
        let row = (
            Uuid::new_v4(),
            "photo.delete".to_owned(),
            Uuid::new_v4(),
            Json(serde_json::json!([])),
            "processing".to_owned(),
            1,
        );
        assert_eq!(map_claimed_row(row), Err(DatabaseError::QueryFailed));
    }
}
