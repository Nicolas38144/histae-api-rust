use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use crate::matches::domain::{
    MATCH_PURGE_DAYS, MatchAvailabilityFailure, MatchCommandResult, MatchRecord, MatchStatus,
};
use crate::matches::store::MatchStoreError;
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub(super) async fn lock_available_match(
    connection: &mut PgConnection,
    match_id: Uuid,
    user_id: Uuid,
) -> Result<MatchCommandResult<MatchRecord>, MatchStoreError> {
    let row = sqlx::query(
        "WITH locked AS MATERIALIZED (
           SELECT id, user1_id, user2_id, status, expires_at, purge_after,
                  continuation_initiator_id, created_at, last_message_at
           FROM match_init
           WHERE id = $1 AND (user1_id = $2 OR user2_id = $2)
             AND NOT EXISTS (
               SELECT 1 FROM user_account
               WHERE user_id IN (match_init.user1_id, match_init.user2_id)
                 AND deleted_at IS NOT NULL
             )
           FOR UPDATE
         )
         SELECT locked.*, clock_timestamp() AS database_now FROM locked",
    )
    .bind(match_id)
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
    let Some(row) = row else {
        return Ok(MatchCommandResult::Unavailable(
            MatchAvailabilityFailure::NotFound,
        ));
    };
    let now: DateTime<Utc> = row
        .try_get("database_now")
        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
    let mut record = map_match_record(&row).map_err(MatchStoreError::from)?;
    if record.status == MatchStatus::Active && record.expires_at <= now {
        let opened = sqlx::query(
            "UPDATE match_init
             SET status = 'awaiting_continuation', expires_at = $2 + INTERVAL '24 hours'
             WHERE id = $1
             RETURNING id, user1_id, user2_id, status, expires_at, purge_after,
                       continuation_initiator_id, created_at, last_message_at",
        )
        .bind(match_id)
        .bind(now)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
        record = map_match_record(&opened).map_err(MatchStoreError::from)?;
    }
    if record.status == MatchStatus::AwaitingContinuation && record.expires_at <= now {
        sqlx::query("UPDATE match_init SET status = 'expired', purge_after = $2 WHERE id = $1")
            .bind(match_id)
            .bind(now + TimeDelta::days(MATCH_PURGE_DAYS))
            .execute(connection)
            .await
            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
        return Ok(MatchCommandResult::Unavailable(
            MatchAvailabilityFailure::Expired,
        ));
    }
    if !matches!(
        record.status,
        MatchStatus::Active | MatchStatus::AwaitingContinuation | MatchStatus::Confirmed
    ) {
        return Ok(MatchCommandResult::Unavailable(
            MatchAvailabilityFailure::InvalidState,
        ));
    }
    Ok(MatchCommandResult::Available(record))
}

pub(super) fn parse_status(value: String) -> Result<MatchStatus, MatchStoreError> {
    MatchStatus::parse(&value).ok_or(MatchStoreError::Database(DatabaseError::QueryFailed))
}

pub(super) fn map_match_record(row: &sqlx::postgres::PgRow) -> Result<MatchRecord, DatabaseError> {
    let status: String = row.try_get("status").map_err(map_sqlx_error)?;
    Ok(MatchRecord {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        user1_id: row.try_get("user1_id").map_err(map_sqlx_error)?,
        user2_id: row.try_get("user2_id").map_err(map_sqlx_error)?,
        status: MatchStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?,
        expires_at: row.try_get("expires_at").map_err(map_sqlx_error)?,
        purge_after: row.try_get("purge_after").map_err(map_sqlx_error)?,
        continuation_initiator_id: row
            .try_get("continuation_initiator_id")
            .map_err(map_sqlx_error)?,
        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
        last_message_at: row.try_get("last_message_at").map_err(map_sqlx_error)?,
    })
}
