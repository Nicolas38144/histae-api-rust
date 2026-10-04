use super::{
    domain::*,
    store::{OutboxAdminFuture, OutboxAdminStore},
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use sqlx::Row as _;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgOutboxAdminRepository {
    database: Database,
}

impl PgOutboxAdminRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn resolve(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
        action: OperatorAction,
    ) -> Result<OperatorResult, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let event = sqlx::query(
                        "SELECT event_type, aggregate_id, status
                         FROM outbox_event WHERE id = $1 FOR UPDATE",
                    )
                    .bind(event_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some(event) = event else {
                        return Ok(OperatorResult::NotFound);
                    };
                    let event_type: String = event.try_get("event_type").map_err(map_sqlx_error)?;
                    let aggregate_id: Uuid =
                        event.try_get("aggregate_id").map_err(map_sqlx_error)?;
                    let status: String = event.try_get("status").map_err(map_sqlx_error)?;
                    if status != "dead_letter" {
                        return Ok(OperatorResult::NotDeadLetter);
                    }
                    if action == OperatorAction::Discard && event_type != "notification.push" {
                        if event_type != "photo.delete" {
                            return Ok(OperatorResult::DiscardNotAllowed);
                        }
                        let photo_exists =
                            sqlx::query_scalar::<_, i32>("SELECT 1 FROM user_photo WHERE id = $1")
                                .bind(aggregate_id)
                                .fetch_optional(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?
                                .is_some();
                        if photo_exists {
                            return Ok(OperatorResult::DiscardNotAllowed);
                        }
                    }
                    sqlx::query(
                        "INSERT INTO outbox_operator_action
                           (outbox_event_id, administrator_id, administrator_role,
                            event_type, action, reason)
                         VALUES ($1, $2, $3, $4, $5, $6)",
                    )
                    .bind(event_id)
                    .bind(operator.user_id)
                    .bind(operator.role.as_str())
                    .bind(&event_type)
                    .bind(action.as_str())
                    .bind(&reason)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    match action {
                        OperatorAction::Retry => {
                            sqlx::query(
                                "UPDATE outbox_event
                                 SET status = 'pending', attempts = 0,
                                   available_at = clock_timestamp(), locked_at = NULL,
                                   locked_by = NULL, last_error_code = NULL,
                                   processed_at = NULL, dead_lettered_at = NULL,
                                   resolved_at = NULL, resolved_by = NULL,
                                   resolution_reason = NULL
                                 WHERE id = $1",
                            )
                            .bind(event_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }
                        OperatorAction::Discard => {
                            sqlx::query(
                                "UPDATE outbox_event
                                 SET status = 'discarded', locked_at = NULL,
                                   locked_by = NULL, processed_at = NULL,
                                   resolved_at = clock_timestamp(), resolved_by = $2,
                                   resolution_reason = $3
                                 WHERE id = $1",
                            )
                            .bind(event_id)
                            .bind(operator.user_id)
                            .bind(&reason)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }
                    }
                    Ok(OperatorResult::Updated)
                })
            })
            .await
    }
}

impl OutboxAdminStore for PgOutboxAdminRepository {
    fn list_dead_letters(
        &self,
        limit: u32,
        cursor: Option<DeadLetterCursor>,
    ) -> OutboxAdminFuture<'_, Vec<DeadLetterRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, event_type, attempts, last_error_code,
                        to_char(created_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at,
                        to_char(dead_lettered_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS dead_lettered_at
                 FROM outbox_event
                 WHERE status = 'dead_letter'
                   AND ($2::timestamptz IS NULL
                     OR (dead_lettered_at, id) < ($2::timestamptz, $3::uuid))
                 ORDER BY dead_lettered_at DESC, id DESC LIMIT $1",
            )
            .bind(i64::from(limit))
            .bind(cursor.map(|value| value.at))
            .bind(cursor.map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|row| {
                    let attempts: i16 = row.try_get("attempts").map_err(map_sqlx_error)?;
                    Ok(DeadLetterRow {
                        id: row.try_get("id").map_err(map_sqlx_error)?,
                        event_type: row.try_get("event_type").map_err(map_sqlx_error)?,
                        attempts: u16::try_from(attempts)
                            .map_err(|_| DatabaseError::QueryFailed)?,
                        last_error_code: row.try_get("last_error_code").map_err(map_sqlx_error)?,
                        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
                        dead_lettered_at: row
                            .try_get("dead_lettered_at")
                            .map_err(map_sqlx_error)?,
                    })
                })
                .collect()
        })
    }

    fn retry_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult> {
        Box::pin(self.resolve(event_id, operator, reason, OperatorAction::Retry))
    }

    fn discard_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult> {
        Box::pin(self.resolve(event_id, operator, reason, OperatorAction::Discard))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperatorAction {
    Retry,
    Discard,
}

impl OperatorAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Discard => "discard",
        }
    }
}
