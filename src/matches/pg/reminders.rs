//! One pre-deadline reminder; delivery rechecks the live conversation state.
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    infra::postgres::{DatabaseError, map_sqlx_error},
    matches::domain::MATCH_FIRST_PERIOD_REMINDER_HOURS,
    notifications::{domain::NotificationIntent, enqueue_notification},
};

pub(super) async fn enqueue_first_period_reminders(
    connection: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<u64, DatabaseError> {
    let candidates = sqlx::query_as::<_, (Uuid, Uuid, Uuid, DateTime<Utc>)>(
        "SELECT m.id,m.user1_id,m.user2_id,m.expires_at FROM match_init m
         JOIN user_account first_account ON first_account.user_id=m.user1_id AND first_account.deleted_at IS NULL AND NOT first_account.is_banned
         JOIN user_account second_account ON second_account.user_id=m.user2_id AND second_account.deleted_at IS NULL AND NOT second_account.is_banned
         WHERE m.status='active' AND m.expires_at>$1
           AND m.expires_at<=$1+($3::bigint*INTERVAL '1 hour')
           AND NOT EXISTS (SELECT 1 FROM user_block WHERE (blocker_id=m.user1_id AND blocked_id=m.user2_id) OR (blocker_id=m.user2_id AND blocked_id=m.user1_id))
           AND NOT EXISTS (SELECT 1 FROM notification n WHERE n.user_id=m.user1_id
             AND n.type='first_period_ending' AND n.payload->>'match_id'=m.id::text AND n.expires_at=m.expires_at)
         ORDER BY m.expires_at,m.id LIMIT $2 FOR UPDATE OF m SKIP LOCKED"
    )
    .bind(now)
    .bind(limit)
    .bind(MATCH_FIRST_PERIOD_REMINDER_HOURS)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let count = candidates.len() as u64;
    for (match_id, first, second, expires_at) in candidates {
        let source = format!("{match_id}:{}", expires_at.timestamp_micros());
        let intent = NotificationIntent::FirstPeriodEnding { match_id, expires_at };
        for participant in [first, second] {
            enqueue_notification(connection, participant, &source, &intent).await?;
        }
    }
    Ok(count)
}
