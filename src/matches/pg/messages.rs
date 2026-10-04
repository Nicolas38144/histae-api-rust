use super::access::lock_available_match;
use crate::infra::postgres::Database;
use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use crate::matches::domain::{
    CursorMessageRow, MatchCommandResult, MessageCreation, MessageCreationResult, MessageRead,
    MessageRecord, PageCursor,
};
use crate::matches::store::MatchStoreError;
use crate::matches::store::{MatchMessageStore, MatchStoreFuture};
use crate::notifications::{domain::NotificationIntent, enqueue_notification};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgMatchMessageRepository {
    database: Database,
}

impl PgMatchMessageRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl MatchMessageStore for PgMatchMessageRepository {
    fn messages_for_user(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Vec<CursorMessageRow>>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if let MatchCommandResult::Unavailable(reason) =
                            lock_available_match(connection, match_id, user_id).await?
                        {
                            return Ok(MatchCommandResult::Unavailable(reason));
                        }
                        let rows = sqlx::query(
                            "SELECT id, match_id, sender_id, content, created_at, read_at,
                               to_char(created_at AT TIME ZONE 'UTC',
                                 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                             FROM chat_message
                             WHERE match_id = $1
                               AND ($4::timestamptz IS NULL
                                 OR (created_at, id) < ($4::timestamptz, $5::uuid))
                             ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
                        )
                        .bind(match_id)
                        .bind(i64::from(limit))
                        .bind(i64::from(offset))
                        .bind(cursor.as_ref().map(|cursor| cursor.at))
                        .bind(cursor.map(|cursor| cursor.id))
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let messages = rows
                            .into_iter()
                            .map(|row| {
                                Ok(CursorMessageRow {
                                    message: map_message_record(&row)?,
                                    cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
                                })
                            })
                            .collect::<Result<Vec<_>, DatabaseError>>()?;
                        Ok(MatchCommandResult::Available(messages))
                    })
                })
                .await
        })
    }

    fn create_message(
        &self,
        message_id: Uuid,
        match_id: Uuid,
        sender_id: Uuid,
        content: String,
        idempotency_key: Uuid,
    ) -> MatchStoreFuture<'_, MessageCreationResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if let Some(replay) =
                            find_idempotent_message(connection, sender_id, idempotency_key).await?
                        {
                            if replay.message.match_id != match_id
                                || replay.message.content != content
                            {
                                return Ok(MessageCreationResult::IdempotencyConflict);
                            }
                            return Ok(MessageCreationResult::Available(MessageCreation {
                                created: false,
                                ..replay
                            }));
                        }
                        let available =
                            match lock_available_match(connection, match_id, sender_id).await? {
                                MatchCommandResult::Available(record) => record,
                                MatchCommandResult::Unavailable(reason) => {
                                    return Ok(MessageCreationResult::Unavailable(reason));
                                }
                            };
                        let inserted = sqlx::query(
                            "INSERT INTO chat_message
                               (id, match_id, sender_id, content, idempotency_key, created_at)
                             VALUES ($1, $2, $3, $4, $5, clock_timestamp())
                             ON CONFLICT (sender_id, idempotency_key)
                               WHERE idempotency_key IS NOT NULL DO NOTHING
                             RETURNING id, match_id, sender_id, content, created_at, read_at",
                        )
                        .bind(message_id)
                        .bind(match_id)
                        .bind(sender_id)
                        .bind(&content)
                        .bind(idempotency_key)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let Some(inserted) = inserted else {
                            let concurrent =
                                find_idempotent_message(connection, sender_id, idempotency_key)
                                    .await?;
                            let Some(concurrent) = concurrent else {
                                return Ok(MessageCreationResult::IdempotencyConflict);
                            };
                            if concurrent.message.match_id != match_id
                                || concurrent.message.content != content
                            {
                                return Ok(MessageCreationResult::IdempotencyConflict);
                            }
                            return Ok(MessageCreationResult::Available(MessageCreation {
                                created: false,
                                ..concurrent
                            }));
                        };
                        let message = map_message_record(&inserted)?;
                        sqlx::query("UPDATE match_init SET last_message_at = $2 WHERE id = $1")
                            .bind(match_id)
                            .bind(message.created_at)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let recipient_id = if available.user1_id == sender_id {
                            available.user2_id
                        } else {
                            available.user1_id
                        };
                        let intent = NotificationIntent::NewMessage {
                            match_id,
                            message_id: message.id,
                            sender_id,
                        };
                        enqueue_notification(
                            connection,
                            recipient_id,
                            &message.id.hyphenated().to_string(),
                            &intent,
                        )
                        .await?;
                        Ok(MessageCreationResult::Available(MessageCreation {
                            message,
                            participant_ids: [available.user1_id, available.user2_id],
                            created: true,
                        }))
                    })
                })
                .await
        })
    }

    fn mark_message_read(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Option<MessageRead>>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let available =
                            match lock_available_match(connection, match_id, user_id).await? {
                                MatchCommandResult::Available(record) => record,
                                MatchCommandResult::Unavailable(reason) => {
                                    return Ok(MatchCommandResult::Unavailable(reason));
                                }
                            };
                        let updated = sqlx::query(
                            "UPDATE chat_message SET read_at = COALESCE(read_at, clock_timestamp())
                             WHERE id = $1 AND match_id = $2 AND sender_id <> $3",
                        )
                        .bind(message_id)
                        .bind(match_id)
                        .bind(user_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let value = (updated.rows_affected() == 1).then_some(MessageRead {
                            updated_count: 1,
                            participant_ids: [available.user1_id, available.user2_id],
                            read_through_message_id: message_id,
                        });
                        Ok(MatchCommandResult::Available(value))
                    })
                })
                .await
        })
    }

    fn mark_messages_read_through(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Option<MessageRead>>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let available =
                            match lock_available_match(connection, match_id, user_id).await? {
                                MatchCommandResult::Available(record) => record,
                                MatchCommandResult::Unavailable(reason) => {
                                    return Ok(MatchCommandResult::Unavailable(reason));
                                }
                            };
                        let row = sqlx::query(
                            "WITH boundary AS (
                               SELECT id, created_at FROM chat_message
                               WHERE id = $2 AND match_id = $1
                             ), updated AS (
                               UPDATE chat_message AS message
                               SET read_at = clock_timestamp()
                               FROM boundary
                               WHERE message.match_id = $1 AND message.sender_id <> $3
                                 AND message.read_at IS NULL
                                 AND (message.created_at, message.id)
                                   <= (boundary.created_at, boundary.id)
                               RETURNING message.id
                             )
                             SELECT EXISTS (SELECT 1 FROM boundary) AS boundary_exists,
                               count(*)::integer AS updated_count
                             FROM updated",
                        )
                        .bind(match_id)
                        .bind(message_id)
                        .bind(user_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        let boundary_exists: bool = row
                            .try_get("boundary_exists")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        if !boundary_exists {
                            return Ok(MatchCommandResult::Available(None));
                        }
                        let updated_count = row
                            .try_get("updated_count")
                            .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
                        Ok(MatchCommandResult::Available(Some(MessageRead {
                            updated_count,
                            participant_ids: [available.user1_id, available.user2_id],
                            read_through_message_id: message_id,
                        })))
                    })
                })
                .await
        })
    }
}

async fn find_idempotent_message(
    connection: &mut PgConnection,
    sender_id: Uuid,
    idempotency_key: Uuid,
) -> Result<Option<MessageCreation>, MatchStoreError> {
    let row = sqlx::query(
        "SELECT message.id, message.match_id, message.sender_id, message.content,
                message.created_at, message.read_at,
                match_record.user1_id, match_record.user2_id
         FROM chat_message AS message
         JOIN match_init AS match_record ON match_record.id = message.match_id
         WHERE message.sender_id = $1 AND message.idempotency_key = $2
           AND NOT EXISTS (
             SELECT 1 FROM user_account
             WHERE user_id IN (match_record.user1_id, match_record.user2_id)
               AND deleted_at IS NOT NULL
           )",
    )
    .bind(sender_id)
    .bind(idempotency_key)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| MatchStoreError::Database(map_sqlx_error(error)))?;
    row.map(|row| {
        Ok::<MessageCreation, DatabaseError>(MessageCreation {
            message: map_message_record(&row)?,
            participant_ids: [
                row.try_get("user1_id").map_err(map_sqlx_error)?,
                row.try_get("user2_id").map_err(map_sqlx_error)?,
            ],
            created: false,
        })
    })
    .transpose()
    .map_err(MatchStoreError::from)
}

fn map_message_record(row: &sqlx::postgres::PgRow) -> Result<MessageRecord, DatabaseError> {
    Ok(MessageRecord {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        match_id: row.try_get("match_id").map_err(map_sqlx_error)?,
        sender_id: row.try_get("sender_id").map_err(map_sqlx_error)?,
        content: row.try_get("content").map_err(map_sqlx_error)?,
        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
        read_at: row.try_get("read_at").map_err(map_sqlx_error)?,
    })
}
