//! Confirmed conversation deadlines share the match lock with message creation.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    infra::postgres::{DatabaseError, map_sqlx_error},
    matches::domain::MATCH_WARNING_DAYS,
    notifications::{domain::NotificationIntent, enqueue_notification},
};

pub(super) async fn enqueue_warnings(
    connection: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    let candidates = sqlx::query_as::<_, (Uuid, Uuid, Uuid, DateTime<Utc>)>(
        "SELECT m.id, m.user1_id, m.user2_id, m.expires_at FROM match_init m
         JOIN user_account a ON a.user_id = m.user1_id AND a.deleted_at IS NULL AND NOT a.is_banned
         JOIN user_account b ON b.user_id = m.user2_id AND b.deleted_at IS NULL AND NOT b.is_banned
         WHERE m.status = 'confirmed' AND m.expires_at > $1
           AND m.expires_at <= $1 + ($3::bigint * INTERVAL '1 day')
           AND NOT EXISTS (
             SELECT 1 FROM user_block
             WHERE (blocker_id = m.user1_id AND blocked_id = m.user2_id)
                OR (blocker_id = m.user2_id AND blocked_id = m.user1_id)
           )
           AND NOT EXISTS (
             SELECT 1 FROM notification n
             WHERE n.user_id = m.user1_id AND n.type = 'match_expiring'
               AND n.payload->>'match_id' = m.id::text
               AND (n.payload->>'expires_at')::timestamptz = m.expires_at
           )
         ORDER BY m.expires_at, m.id LIMIT $2 FOR UPDATE OF m SKIP LOCKED",
    )
    .bind(now)
    .bind(limit)
    .bind(MATCH_WARNING_DAYS)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let count = candidates.len() as u64;
    for (match_id, user1_id, user2_id, expires_at) in candidates {
        let source = format!("{match_id}:{}", expires_at.timestamp_micros());
        let intent = NotificationIntent::MatchExpiring {
            match_id,
            expires_at,
        };
        for user_id in [user1_id, user2_id] {
            enqueue_notification(connection, user_id, &source, &intent).await?;
        }
    }
    Ok(count)
}

pub(super) async fn close_inactive_matches(
    connection: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    sqlx::query(
        "WITH candidates AS MATERIALIZED (
           SELECT id FROM match_init WHERE status = 'confirmed' AND expires_at <= $1
           ORDER BY expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )
         UPDATE match_init m SET status = 'ended', purge_after = $1
         FROM candidates c WHERE m.id = c.id",
    )
    .bind(now)
    .bind(limit)
    .execute(connection)
    .await
    .map(|result| result.rows_affected())
    .map_err(map_sqlx_error)
}
