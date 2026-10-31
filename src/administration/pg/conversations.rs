//! Audited conversation reads. Report eligibility is checked on every page;
//! both participant audits and message reads belong to the same transaction.
use std::sync::LazyLock;

use sqlx::Row as _;
use uuid::Uuid;

use super::{PgAdministrationStore, record_audit};
use crate::{
    administration::domain::{CursorMatchRow, CursorMessageRow, PageCursor},
    identity::admin_role::AdminRole,
    infra::postgres::{DatabaseError, map_sqlx_error},
    matches::domain::{MatchRecord, MatchStatus, MessageRecord},
};

#[allow(clippy::too_many_arguments)]
impl PgAdministrationStore {
    pub(super) async fn list_conversations(
        &self,
        user_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> Result<Option<Vec<CursorMatchRow>>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL)",
                    )
                    .bind(user_id)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if !exists {
                        return Ok(None);
                    }
                    let rows = sqlx::query(LIST_MATCHES_SQL.as_str())
                        .bind(user_id)
                        .bind(i64::from(limit))
                        .bind(i64::from(offset))
                        .bind(cursor.as_ref().map(|value| value.at))
                        .bind(cursor.as_ref().map(|value| value.id))
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .into_iter()
                        .map(map_match)
                        .collect::<Result<Vec<_>, _>>()?;
                    record_audit(
                        connection,
                        user_id,
                        admin_id,
                        admin_role,
                        "view_matches",
                        &reason,
                    )
                    .await?;
                    Ok(Some(rows))
                })
            })
            .await
    }

    pub(super) async fn read_conversation(
        &self,
        match_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> Result<Option<Vec<CursorMessageRow>>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    // Recheck on every read, before loading messages or recording access.
                    // Missing and unreported matches are deliberately indistinguishable.
                    let participants = sqlx::query(MESSAGE_PARTICIPANTS_SQL.as_str())
                        .bind(match_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    let Some(participants) = participants else {
                        return Ok(None);
                    };
                    let user1_id: Uuid =
                        participants.try_get("user1_id").map_err(map_sqlx_error)?;
                    let user2_id: Uuid =
                        participants.try_get("user2_id").map_err(map_sqlx_error)?;
                    record_audit(
                        connection,
                        user1_id,
                        admin_id,
                        admin_role,
                        "view_messages",
                        &reason,
                    )
                    .await?;
                    record_audit(
                        connection,
                        user2_id,
                        admin_id,
                        admin_role,
                        "view_messages",
                        &reason,
                    )
                    .await?;
                    let rows = sqlx::query(
                        "SELECT id, match_id, sender_id, content, created_at, read_at,
                               to_char(created_at AT TIME ZONE 'UTC',
                                 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                             FROM chat_message WHERE match_id = $1
                               AND ($4::timestamptz IS NULL OR
                                    (created_at, id) < ($4::timestamptz, $5::uuid))
                             ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
                    )
                    .bind(match_id)
                    .bind(i64::from(limit))
                    .bind(i64::from(offset))
                    .bind(cursor.as_ref().map(|value| value.at))
                    .bind(cursor.as_ref().map(|value| value.id))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    .into_iter()
                    .map(map_message)
                    .collect::<Result<Vec<_>, _>>()?;
                    Ok(Some(rows))
                })
            })
            .await
    }
}

fn map_match(row: sqlx::postgres::PgRow) -> Result<CursorMatchRow, DatabaseError> {
    let status: String = row.try_get("status").map_err(map_sqlx_error)?;
    Ok(CursorMatchRow {
        can_view_messages: row.try_get("can_view_messages").map_err(map_sqlx_error)?,
        item: MatchRecord {
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
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn map_message(row: sqlx::postgres::PgRow) -> Result<CursorMessageRow, DatabaseError> {
    Ok(CursorMessageRow {
        item: MessageRecord {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            match_id: row.try_get("match_id").map_err(map_sqlx_error)?,
            sender_id: row.try_get("sender_id").map_err(map_sqlx_error)?,
            content: row.try_get("content").map_err(map_sqlx_error)?,
            created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
            read_at: row.try_get("read_at").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

// A profile report or a report from another match must never unlock this conversation.
const MATCH_REPORT_EXISTS_SQL: &str = r#"
EXISTS (SELECT 1 FROM user_report AS report
  WHERE report.match_id = match_record.id
    AND ((report.reporter_id = match_record.user1_id AND report.reported_id = match_record.user2_id)
      OR (report.reporter_id = match_record.user2_id AND report.reported_id = match_record.user1_id)))
"#;

static MESSAGE_PARTICIPANTS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT user1_id, user2_id FROM match_init AS match_record
         WHERE match_record.id = $1 AND {MATCH_REPORT_EXISTS_SQL}"
    )
});

static LIST_MATCHES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
WITH page AS MATERIALIZED (
  SELECT match_record.*,
    COALESCE(match_record.last_message_at, match_record.created_at) AS activity_at
  FROM match_init AS match_record
  WHERE (match_record.user1_id = $1 OR match_record.user2_id = $1)
    AND ($4::timestamptz IS NULL OR
      (COALESCE(match_record.last_message_at, match_record.created_at), match_record.id)
        < ($4::timestamptz, $5::uuid))
  ORDER BY activity_at DESC, match_record.id DESC LIMIT $2 OFFSET $3
)
SELECT id, user1_id, user2_id, status, expires_at, purge_after,
  continuation_initiator_id, created_at, last_message_at,
  {MATCH_REPORT_EXISTS_SQL} AS can_view_messages,
  to_char(activity_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM page AS match_record ORDER BY activity_at DESC, id DESC
"#
    )
});
