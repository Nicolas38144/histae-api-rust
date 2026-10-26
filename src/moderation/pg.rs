use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::domain::{
    ModerationContentType, ModerationReviewInput, ModerationReviewResult, ModerationRow, PageCursor,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::outbox::pg::PgOutboxRepository;
use crate::outbox::types::{NewOutboxEvent, OutboxEventType};
use crate::profiles::domain::{ModerationReason, ModerationStatus};

use super::store::{ModerationStore, ModerationStoreFuture};

#[derive(Clone)]
pub struct PgModerationRepository {
    database: Database,
    outbox: PgOutboxRepository,
}

impl PgModerationRepository {
    pub fn new(database: Database, outbox: PgOutboxRepository) -> Self {
        Self { database, outbox }
    }
}

impl ModerationStore for PgModerationRepository {
    fn list<'a>(
        &'a self,
        status: Option<ModerationStatus>,
        content_type: Option<ModerationContentType>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> ModerationStoreFuture<'a, Vec<ModerationRow>> {
        Box::pin(async move {
            let mut query = QueryBuilder::<Postgres>::new(
                "WITH page AS MATERIALIZED (
                  SELECT * FROM content_moderation_case WHERE true",
            );
            if let Some(status) = status {
                query.push(" AND status = ").push_bind(status.as_str());
            }
            if let Some(content_type) = content_type {
                query
                    .push(" AND content_type = ")
                    .push_bind(content_type.as_str());
            }
            if let Some(cursor) = cursor {
                query
                    .push(" AND (updated_at, id) < (")
                    .push_bind(cursor.at)
                    .push(", ")
                    .push_bind(cursor.id)
                    .push(")");
            }
            query
                .push(" ORDER BY updated_at DESC, id DESC LIMIT ")
                .push_bind(i64::from(limit))
                .push(" OFFSET ")
                .push_bind(i64::from(offset))
                .push(") ")
                .push(LIST_COLUMNS_QUERY);
            let rows = query
                .build()
                .fetch_all(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
            rows.iter().map(map_row).collect()
        })
    }

    fn detail(
        &self,
        case_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
    ) -> ModerationStoreFuture<'_, Option<ModerationRow>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let row = sqlx::query(DETAIL_QUERY)
                            .bind(case_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        let Some(row) = row else {
                            return Ok(None);
                        };
                        let mapped = map_row(&row)?;
                        record_audit(
                            connection,
                            mapped.user_id,
                            admin_id,
                            admin_role,
                            "view_moderation_content",
                            &reason,
                        )
                        .await?;
                        Ok(Some(mapped))
                    })
                })
                .await
        })
    }

    fn review(
        &self,
        case_id: Uuid,
        input: ModerationReviewInput,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> ModerationStoreFuture<'_, ModerationReviewResult> {
        let outbox = self.outbox.clone();
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        type Current = (Uuid, String, Option<Uuid>, Option<String>, i32);
                        let current = sqlx::query_as::<_, Current>(
                            "SELECT moderation.user_id, moderation.content_type,
                                    moderation.photo_id, photo.status, moderation.version
                             FROM content_moderation_case AS moderation
                             LEFT JOIN user_photo AS photo ON photo.id = moderation.photo_id
                             WHERE moderation.id = $1
                             FOR UPDATE OF moderation",
                        )
                        .bind(case_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some((user_id, content_type, photo_id, photo_status, version)) =
                            current
                        else {
                            return Ok(ModerationReviewResult::NotFound);
                        };
                        if version != input.version {
                            return Ok(ModerationReviewResult::Stale);
                        }
                        let is_photo = content_type == "photo";
                        if is_photo && photo_status.as_deref() != Some("ready") {
                            return Ok(ModerationReviewResult::NotActionable);
                        }
                        let updated = sqlx::query(
                            "UPDATE content_moderation_case
                             SET status = $2, reviewed_by = $3,
                                 reviewed_at = clock_timestamp(), review_reason = $4,
                                 version = version + 1, updated_at = clock_timestamp()
                             WHERE id = $1 AND version = $5",
                        )
                        .bind(case_id)
                        .bind(input.decision.as_str())
                        .bind(admin_id)
                        .bind(&input.reason)
                        .bind(input.version)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if updated.rows_affected() != 1 {
                            return Ok(ModerationReviewResult::Stale);
                        }
                        if is_photo && input.decision.as_str() == "rejected" {
                            let photo_id = photo_id.ok_or(DatabaseError::QueryFailed)?;
                            sqlx::query(
                                "UPDATE user_photo
                                 SET status = 'deleting', updated_at = clock_timestamp()
                                 WHERE id = $1 AND status = 'ready'",
                            )
                            .bind(photo_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            outbox
                                .requeue(
                                    connection,
                                    &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, photo_id),
                                )
                                .await?;
                        }
                        record_audit(
                            connection,
                            user_id,
                            admin_id,
                            admin_role,
                            "admin_review_content",
                            &input.reason,
                        )
                        .await?;
                        Ok(ModerationReviewResult::Updated)
                    })
                })
                .await
        })
    }
}

const LIST_COLUMNS_QUERY: &str = concat!(
    "SELECT ",
    "moderation.id, moderation.user_id, profile.firstname,
     moderation.content_type, moderation.status, moderation.reason_codes,
     moderation.policy_version, moderation.version, moderation.review_reason,
     moderation.reviewed_at, moderation.reviewed_by,
     moderation.created_at, moderation.updated_at,
     to_char(moderation.updated_at AT TIME ZONE 'UTC',
       'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
     NULL::text AS text_content, NULL::text AS question, NULL::text AS object_key
     FROM page AS moderation
     LEFT JOIN user_profile AS profile ON profile.user_id = moderation.user_id
     ORDER BY moderation.updated_at DESC, moderation.id DESC"
);

const DETAIL_QUERY: &str = concat!(
    "SELECT ",
    "moderation.id, moderation.user_id, profile.firstname,
     moderation.content_type, moderation.status, moderation.reason_codes,
     moderation.policy_version, moderation.version, moderation.review_reason,
     moderation.reviewed_at, moderation.reviewed_by,
     moderation.created_at, moderation.updated_at,
     to_char(moderation.updated_at AT TIME ZONE 'UTC',
       'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
     CASE moderation.content_type
       WHEN 'bio' THEN bio.bio
       WHEN 'profile_answer' THEN answer.answer
       ELSE NULL
     END AS text_content,
     question.prompt AS question, photo.object_key
     FROM content_moderation_case AS moderation
     LEFT JOIN user_profile AS profile ON profile.user_id = moderation.user_id
     LEFT JOIN user_profile AS bio ON bio.user_id = moderation.bio_user_id
     LEFT JOIN user_profile_answer AS answer ON answer.id = moderation.profile_answer_id
     LEFT JOIN profile_question AS question ON question.id = answer.question_id
     LEFT JOIN user_photo AS photo ON photo.id = moderation.photo_id
     WHERE moderation.id = $1"
);

fn map_row(row: &sqlx::postgres::PgRow) -> Result<ModerationRow, DatabaseError> {
    let content_type =
        ModerationContentType::parse(row.try_get("content_type").map_err(map_sqlx_error)?)
            .ok_or(DatabaseError::QueryFailed)?;
    let status = ModerationStatus::parse(row.try_get("status").map_err(map_sqlx_error)?)
        .ok_or(DatabaseError::QueryFailed)?;
    let reason_codes = row
        .try_get::<Vec<String>, _>("reason_codes")
        .map_err(map_sqlx_error)?
        .into_iter()
        .map(|value| ModerationReason::parse(&value).ok_or(DatabaseError::QueryFailed))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ModerationRow {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
        firstname: row.try_get("firstname").map_err(map_sqlx_error)?,
        content_type,
        status,
        reason_codes,
        policy_version: row.try_get("policy_version").map_err(map_sqlx_error)?,
        version: row.try_get("version").map_err(map_sqlx_error)?,
        review_reason: row.try_get("review_reason").map_err(map_sqlx_error)?,
        reviewed_at: row.try_get("reviewed_at").map_err(map_sqlx_error)?,
        reviewed_by: row.try_get("reviewed_by").map_err(map_sqlx_error)?,
        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
        updated_at: row.try_get("updated_at").map_err(map_sqlx_error)?,
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
        text_content: row.try_get("text_content").map_err(map_sqlx_error)?,
        question: row.try_get("question").map_err(map_sqlx_error)?,
        object_key: row.try_get("object_key").map_err(map_sqlx_error)?,
    })
}

async fn record_audit(
    connection: &mut PgConnection,
    accessed_user_id: Uuid,
    admin_id: Uuid,
    admin_role: AdminRole,
    action: &'static str,
    reason: &str,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO data_access_log
         (accessed_user_id, accessor_id, accessor_role, action, reason)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(accessed_user_id)
    .bind(admin_id)
    .bind(admin_role.as_str())
    .bind(action)
    .bind(reason)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}
