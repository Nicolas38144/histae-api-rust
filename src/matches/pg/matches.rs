use super::access::{lock_available_match, map_match_record, parse_status};
use crate::infra::postgres::Database;
use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use crate::matches::domain::{
    ContinuationResult, EffectivePlan, LastMessageRow, MATCH_PURGE_DAYS, MatchAvailabilityFailure,
    MatchCommandResult, MatchRecord, MatchStatus, PageCursor, UserMatchRow,
};
use crate::matches::store::MatchStoreError;
use crate::matches::store::{MatchStore, MatchStoreFuture};
use crate::notifications::{domain::NotificationIntent, enqueue_notification};
use crate::profiles::domain::Sex;
use chrono::NaiveDate;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgMatchRepository {
    database: Database,
}

impl PgMatchRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl MatchStore for PgMatchRepository {
    fn create(&self, record: MatchRecord) -> MatchStoreFuture<'_, ()> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let inserted = sqlx::query(
                            "INSERT INTO match_init
                             (id, user1_id, user2_id, status, expires_at, purge_after,
                              continuation_initiator_id, created_at, last_message_at)
                             SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9
                             FROM user_account AS first_account
                             JOIN user_account AS second_account ON second_account.user_id = $3
                             WHERE first_account.user_id = $2
                               AND first_account.deleted_at IS NULL
                               AND first_account.is_banned = false
                               AND second_account.deleted_at IS NULL
                               AND second_account.is_banned = false
                               AND NOT EXISTS (
                                 SELECT 1 FROM user_block
                                 WHERE (blocker_id = $2 AND blocked_id = $3)
                                    OR (blocker_id = $3 AND blocked_id = $2)
                               )",
                        )
                        .bind(record.id)
                        .bind(record.user1_id)
                        .bind(record.user2_id)
                        .bind(record.status.as_str())
                        .bind(record.expires_at)
                        .bind(record.purge_after)
                        .bind(record.continuation_initiator_id)
                        .bind(record.created_at)
                        .bind(record.last_message_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        if inserted.rows_affected() != 1 {
                            let blocked: bool = sqlx::query_scalar(
                                "SELECT EXISTS (
                                   SELECT 1 FROM user_block
                                   WHERE (blocker_id = $1 AND blocked_id = $2)
                                      OR (blocker_id = $2 AND blocked_id = $1)
                                 )",
                            )
                            .bind(record.user1_id)
                            .bind(record.user2_id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            return Err(if blocked {
                                MatchStoreError::Blocked
                            } else {
                                MatchStoreError::ParticipantUnavailable
                            });
                        }
                        sqlx::query(
                            "INSERT INTO match_state (match_id, user_id, revealed, continued)
                             VALUES ($1, $2, false, false), ($1, $3, false, false)",
                        )
                        .bind(record.id)
                        .bind(record.user1_id)
                        .bind(record.user2_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let source_id = record.id.hyphenated().to_string();
                        let intent = NotificationIntent::NewMatch {
                            match_id: record.id,
                        };
                        enqueue_notification(connection, record.user1_id, &source_id, &intent)
                            .await?;
                        enqueue_notification(connection, record.user2_id, &source_id, &intent)
                            .await?;
                        Ok(())
                    })
                })
                .await
        })
    }

    fn find_by_pair(
        &self,
        user1_id: Uuid,
        user2_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<MatchRecord>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                "SELECT id, user1_id, user2_id, status, expires_at, purge_after,
                        continuation_initiator_id, created_at, last_message_at
                 FROM match_init WHERE user1_id = $1 AND user2_id = $2",
            )
            .bind(user1_id)
            .bind(user2_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .map(|row| map_match_record(&row))
            .transpose()
            .map_err(MatchStoreError::from)
        })
    }

    fn list_for_user(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> MatchStoreFuture<'_, Vec<UserMatchRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let rows = sqlx::query(LIST_FOR_USER_SQL)
                .bind(user_id)
                .bind(i64::from(limit))
                .bind(i64::from(offset))
                .bind(cursor.as_ref().map(|cursor| cursor.at))
                .bind(cursor.map(|cursor| cursor.id))
                .fetch_all(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(map_user_match_row)
                .collect::<Result<Vec<_>, _>>()
                .map_err(MatchStoreError::from)
        })
    }

    fn record_reveal(
        &self,
        match_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<bool>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if let MatchCommandResult::Unavailable(reason) =
                            lock_available_match(connection, match_id, user_id).await?
                        {
                            return Ok(MatchCommandResult::Unavailable(reason));
                        }
                        let row = sqlx::query(
                            "WITH updated AS (
                               UPDATE match_state SET revealed = true
                               WHERE match_id = $1 AND user_id = $2
                               RETURNING match_id
                             )
                             SELECT EXISTS (SELECT 1 FROM updated) AS updated,
                               COALESCE((
                                 SELECT count(*) = 2
                                   AND bool_and(state.revealed OR state.user_id = $2)
                                 FROM match_state AS state
                                 WHERE state.match_id = (SELECT match_id FROM updated)
                               ), false) AS revealed",
                        )
                        .bind(match_id)
                        .bind(user_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let updated: bool = row
                            .try_get("updated")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        if !updated {
                            return Ok(MatchCommandResult::Unavailable(
                                MatchAvailabilityFailure::NotFound,
                            ));
                        }
                        let revealed = row
                            .try_get("revealed")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        Ok(MatchCommandResult::Available(revealed))
                    })
                })
                .await
        })
    }

    fn participant_ids(
        &self,
        match_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<[Uuid; 2]>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                "SELECT user1_id, user2_id FROM match_init
                 WHERE id = $1 AND (user1_id = $2 OR user2_id = $2)",
            )
            .bind(match_id)
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                Ok::<[Uuid; 2], DatabaseError>([
                    row.try_get("user1_id").map_err(map_sqlx_error)?,
                    row.try_get("user2_id").map_err(map_sqlx_error)?,
                ])
            })
            .transpose()
            .map_err(MatchStoreError::from)
        })
    }

    fn effective_plan(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> MatchStoreFuture<'_, EffectivePlan> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            effective_plan_on(&mut connection, user_id, now).await
        })
    }

    fn continuation_usage(
        &self,
        user_id: Uuid,
        week_start: NaiveDate,
    ) -> MatchStoreFuture<'_, i32> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let used = sqlx::query_scalar::<_, i16>(
                "SELECT used_count FROM continuation_usage
                 WHERE user_id = $1 AND week_start = $2",
            )
            .bind(user_id)
            .bind(week_start)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .unwrap_or(0);
            Ok(i32::from(used))
        })
    }

    fn record_continuation(
        &self,
        match_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, ContinuationResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let header = sqlx::query(
                            "WITH locked AS MATERIALIZED (
                               SELECT status, expires_at, continuation_initiator_id
                               FROM match_init
                               WHERE id = $1 AND (user1_id = $2 OR user2_id = $2)
                               FOR UPDATE
                             )
                             SELECT locked.*, clock_timestamp() AS database_now FROM locked",
                        )
                        .bind(match_id)
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let Some(header) = header else {
                            return Ok(ContinuationResult::NotFound);
                        };
                        let now: DateTime<Utc> = header
                            .try_get("database_now")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let mut status = parse_status(
                            header
                                .try_get("status")
                                .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?,
                        )?;
                        let mut expires_at: DateTime<Utc> = header
                            .try_get("expires_at")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let mut initiator: Option<Uuid> = header
                            .try_get("continuation_initiator_id")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;

                        if status == MatchStatus::Active {
                            if expires_at > now {
                                return Ok(ContinuationResult::NotAvailableYet);
                            }
                            let opened = sqlx::query(
                                "UPDATE match_init
                                 SET status = 'awaiting_continuation',
                                     expires_at = $2 + INTERVAL '24 hours'
                                 WHERE id = $1
                                 RETURNING status, expires_at, continuation_initiator_id",
                            )
                            .bind(match_id)
                            .bind(now)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            status = parse_status(
                                opened.try_get("status").map_err(|error| {
                                    MatchStoreError::Database(map_sqlx_error(error))
                                })?,
                            )?;
                            expires_at = opened.try_get("expires_at").map_err(|error| {
                                MatchStoreError::Database(map_sqlx_error(error))
                            })?;
                            initiator = opened.try_get("continuation_initiator_id").map_err(
                                |error| MatchStoreError::Database(map_sqlx_error(error)),
                            )?;
                        }
                        if status != MatchStatus::AwaitingContinuation {
                            return Ok(ContinuationResult::InvalidState);
                        }
                        if expires_at <= now {
                            sqlx::query(
                                "UPDATE match_init
                                 SET status = 'expired', purge_after = $2
                                 WHERE id = $1",
                            )
                            .bind(match_id)
                            .bind(now + TimeDelta::days(MATCH_PURGE_DAYS))
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            return Ok(ContinuationResult::Expired);
                        }
                        let continued = sqlx::query_scalar::<_, bool>(
                            "SELECT continued FROM match_state
                             WHERE match_id = $1 AND user_id = $2 FOR UPDATE",
                        )
                        .bind(match_id)
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let Some(continued) = continued else {
                            return Ok(ContinuationResult::NotFound);
                        };
                        if continued {
                            return Ok(ContinuationResult::AlreadyRecorded);
                        }
                        let Some(initiator) = initiator else {
                            sqlx::query(
                                "UPDATE match_state SET continued = true
                                 WHERE match_id = $1 AND user_id = $2",
                            )
                            .bind(match_id)
                            .bind(user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            sqlx::query(
                                "UPDATE match_init SET continuation_initiator_id = $2 WHERE id = $1",
                            )
                            .bind(match_id)
                            .bind(user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            return Ok(ContinuationResult::Pending);
                        };

                        let plan = effective_plan_on(connection, initiator, now).await?;
                        if let Some(limit) = plan.weekly_limit {
                            let usage = sqlx::query_scalar::<_, i16>(
                                "INSERT INTO continuation_usage (user_id, week_start, used_count)
                                 SELECT $1, date_trunc('week', $2 AT TIME ZONE 'UTC')::date, 1
                                 WHERE $3 > 0
                                 ON CONFLICT (user_id, week_start) DO UPDATE
                                   SET used_count = continuation_usage.used_count + 1
                                 WHERE continuation_usage.used_count < $3
                                 RETURNING used_count",
                            )
                            .bind(initiator)
                            .bind(now)
                            .bind(limit)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                            if usage.is_none() {
                                return Ok(ContinuationResult::QuotaReached);
                            }
                        }
                        sqlx::query(
                            "UPDATE match_state SET continued = true
                             WHERE match_id = $1 AND user_id = $2",
                        )
                        .bind(match_id)
                        .bind(user_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let confirmed = sqlx::query(
                            "UPDATE match_init SET status = 'confirmed', purge_after = NULL
                             WHERE id = $1 AND status = 'awaiting_continuation'
                               AND expires_at > $2
                               AND (SELECT count(*) FROM match_state
                                    WHERE match_id = $1 AND continued = true) = 2",
                        )
                        .bind(match_id)
                        .bind(now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        Ok(if confirmed.rows_affected() == 1 {
                            ContinuationResult::Confirmed
                        } else {
                            ContinuationResult::InvalidState
                        })
                    })
                })
                .await
        })
    }
}

async fn effective_plan_on(
    connection: &mut PgConnection,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<EffectivePlan, MatchStoreError> {
    let row = sqlx::query(
        "SELECT plan.code, plan.weekly_continuation_limit
         FROM subscription_plan AS plan
         WHERE plan.code = COALESCE((
           SELECT subscription.plan
           FROM user_subscription AS subscription
           WHERE subscription.user_id = $1
             AND (
               subscription.plan = 'free'
               OR (
                 (subscription.provider IS NULL
                  OR subscription.status IN ('trialing', 'active', 'past_due'))
                 AND (subscription.current_period_ends_at IS NULL
                      OR subscription.current_period_ends_at > $2)
               )
             )
         ), 'free')",
    )
    .bind(user_id)
    .bind(now)
    .fetch_optional(connection)
    .await
    .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?
    .ok_or(MatchStoreError::Database(DatabaseError::RowNotFound))?;
    Ok(EffectivePlan {
        plan: row
            .try_get("code")
            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?,
        weekly_limit: row
            .try_get("weekly_continuation_limit")
            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?,
    })
}

fn map_user_match_row(row: sqlx::postgres::PgRow) -> Result<UserMatchRow, DatabaseError> {
    let other_sex = row
        .try_get::<Option<String>, _>("other_sex")
        .map_err(map_sqlx_error)?
        .map(|value| Sex::parse(&value).ok_or(DatabaseError::QueryFailed))
        .transpose()?;
    let answers: Value = row
        .try_get("other_profile_answers")
        .map_err(map_sqlx_error)?;
    let other_profile_answers = answers
        .as_array()
        .cloned()
        .ok_or(DatabaseError::QueryFailed)?;
    let last_message_id: Option<Uuid> = row.try_get("last_message_id").map_err(map_sqlx_error)?;
    let last_message = last_message_id
        .map(|id| {
            Ok(LastMessageRow {
                id,
                sender_id: row
                    .try_get("last_message_sender_id")
                    .map_err(map_sqlx_error)?,
                content: row
                    .try_get("last_message_content")
                    .map_err(map_sqlx_error)?,
                created_at: row
                    .try_get("last_message_created_at")
                    .map_err(map_sqlx_error)?,
                read_at: row
                    .try_get("last_message_read_at")
                    .map_err(map_sqlx_error)?,
            })
        })
        .transpose()?;
    Ok(UserMatchRow {
        record: map_match_record(&row)?,
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
        other_user_id: row.try_get("other_user_id").map_err(map_sqlx_error)?,
        other_firstname: row.try_get("other_firstname").map_err(map_sqlx_error)?,
        other_age: row.try_get("other_age").map_err(map_sqlx_error)?,
        other_sex,
        other_bio: row.try_get("other_bio").map_err(map_sqlx_error)?,
        other_photo: row.try_get("other_photo").map_err(map_sqlx_error)?,
        other_traits: row.try_get("other_traits").map_err(map_sqlx_error)?,
        other_profile_answers,
        my_revealed: row.try_get("my_revealed").map_err(map_sqlx_error)?,
        photos_revealed: row.try_get("photos_revealed").map_err(map_sqlx_error)?,
        my_continued: row.try_get("my_continued").map_err(map_sqlx_error)?,
        unread_count: row.try_get("unread_count").map_err(map_sqlx_error)?,
        last_message,
    })
}

const LIST_FOR_USER_SQL: &str = r#"
WITH page AS MATERIALIZED (
  SELECT participant_matches.*
  FROM (
    (SELECT match_record.id, match_record.user1_id, match_record.user2_id,
      match_record.status, match_record.expires_at, match_record.purge_after,
      match_record.continuation_initiator_id, match_record.created_at,
      match_record.last_message_at, match_record.user2_id AS other_user_id,
      COALESCE(match_record.last_message_at, match_record.created_at) AS activity_at
    FROM match_init AS match_record
    WHERE match_record.user1_id = $1 AND match_record.status <> 'ended'
      AND EXISTS (SELECT 1 FROM user_account
                  WHERE user_id = match_record.user2_id AND deleted_at IS NULL)
      AND NOT EXISTS (
        SELECT 1 FROM user_block
        WHERE (blocker_id = $1 AND blocked_id = match_record.user2_id)
           OR (blocker_id = match_record.user2_id AND blocked_id = $1)
      )
      AND ($4::timestamptz IS NULL OR
        (COALESCE(match_record.last_message_at, match_record.created_at), match_record.id)
          < ($4::timestamptz, $5::uuid))
    ORDER BY COALESCE(match_record.last_message_at, match_record.created_at) DESC,
      match_record.id DESC
    LIMIT ($2::bigint + $3::bigint))
    UNION ALL
    (SELECT match_record.id, match_record.user1_id, match_record.user2_id,
      match_record.status, match_record.expires_at, match_record.purge_after,
      match_record.continuation_initiator_id, match_record.created_at,
      match_record.last_message_at, match_record.user1_id AS other_user_id,
      COALESCE(match_record.last_message_at, match_record.created_at) AS activity_at
    FROM match_init AS match_record
    WHERE match_record.user2_id = $1 AND match_record.status <> 'ended'
      AND EXISTS (SELECT 1 FROM user_account
                  WHERE user_id = match_record.user1_id AND deleted_at IS NULL)
      AND NOT EXISTS (
        SELECT 1 FROM user_block
        WHERE (blocker_id = $1 AND blocked_id = match_record.user1_id)
           OR (blocker_id = match_record.user1_id AND blocked_id = $1)
      )
      AND ($4::timestamptz IS NULL OR
        (COALESCE(match_record.last_message_at, match_record.created_at), match_record.id)
          < ($4::timestamptz, $5::uuid))
    ORDER BY COALESCE(match_record.last_message_at, match_record.created_at) DESC,
      match_record.id DESC
    LIMIT ($2::bigint + $3::bigint))
  ) AS participant_matches
  ORDER BY activity_at DESC, id DESC
  LIMIT $2 OFFSET $3
)
SELECT match_record.id, match_record.user1_id, match_record.user2_id,
  match_record.status, match_record.expires_at, match_record.purge_after,
  match_record.continuation_initiator_id, match_record.created_at,
  match_record.last_message_at, other_profile.user_id AS other_user_id,
  other_profile.firstname AS other_firstname,
  date_part('year', age(current_date, other_profile.birthdate))::integer AS other_age,
  other_profile.sex AS other_sex,
  CASE WHEN other_bio_moderation.status = 'approved'
    THEN other_profile.bio ELSE NULL END AS other_bio,
  CASE WHEN COALESCE(my_state.revealed, false)
         AND COALESCE(other_state.revealed, false)
    THEN other_photo.object_key ELSE NULL END AS other_photo,
  COALESCE(other_traits.names, ARRAY[]::text[]) AS other_traits,
  COALESCE(other_answers.items, '[]'::jsonb) AS other_profile_answers,
  COALESCE(my_state.revealed, false) AS my_revealed,
  COALESCE(my_state.revealed, false)
    AND COALESCE(other_state.revealed, false) AS photos_revealed,
  COALESCE(my_state.continued, false) AS my_continued,
  COALESCE(unread.count, 0)::integer AS unread_count,
  latest.id AS last_message_id, latest.sender_id AS last_message_sender_id,
  latest.content AS last_message_content,
  latest.created_at AS last_message_created_at,
  latest.read_at AS last_message_read_at,
  to_char(COALESCE(match_record.last_message_at, match_record.created_at)
    AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM page AS match_record
JOIN user_profile AS other_profile
  ON other_profile.user_id = match_record.other_user_id
LEFT JOIN user_photo AS other_photo
  ON other_photo.user_id = other_profile.user_id AND other_photo.status = 'ready'
  AND EXISTS (
    SELECT 1 FROM content_moderation_case
    WHERE photo_id = other_photo.id AND status = 'approved'
  )
LEFT JOIN content_moderation_case AS other_bio_moderation
  ON other_bio_moderation.bio_user_id = other_profile.user_id
LEFT JOIN match_state AS my_state
  ON my_state.match_id = match_record.id AND my_state.user_id = $1
LEFT JOIN match_state AS other_state
  ON other_state.match_id = match_record.id
  AND other_state.user_id = other_profile.user_id
LEFT JOIN LATERAL (
  SELECT array_agg(trait.name ORDER BY trait.name) AS names
  FROM user_trait JOIN trait ON trait.id = user_trait.trait_id
  WHERE user_trait.user_id = other_profile.user_id
) AS other_traits ON true
LEFT JOIN LATERAL (
  SELECT jsonb_agg(jsonb_build_object(
    'question_id', answer.question_id,
    'code', question.code,
    'question', question.prompt,
    'answer', answer.answer,
    'position', answer.position
  ) ORDER BY answer.position) AS items
  FROM user_profile_answer AS answer
  JOIN profile_question AS question ON question.id = answer.question_id
  JOIN content_moderation_case AS moderation
    ON moderation.profile_answer_id = answer.id AND moderation.status = 'approved'
  WHERE answer.user_id = other_profile.user_id
) AS other_answers ON true
LEFT JOIN LATERAL (
  SELECT message.id, message.sender_id, message.content,
         message.created_at, message.read_at
  FROM chat_message AS message
  WHERE message.match_id = match_record.id
  ORDER BY message.created_at DESC, message.id DESC
  LIMIT 1
) AS latest ON true
LEFT JOIN LATERAL (
  SELECT count(*)::integer AS count
  FROM chat_message AS message
  WHERE message.match_id = match_record.id
    AND message.sender_id <> $1 AND message.read_at IS NULL
) AS unread ON true
ORDER BY match_record.activity_at DESC, match_record.id DESC
"#;
