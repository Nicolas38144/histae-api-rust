use super::{
    AdminPhotoReconciliation, AdminPhotoRow, AdminPhotoStore, AdminPhotoStoreFuture,
    PhotoReconciliationFilter, ReconciliationResult,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::moderation::domain::{PageCursor, wire_timestamp};
use crate::outbox::{
    pg::PgOutboxRepository,
    types::{NewOutboxEvent, OutboxEventType},
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
#[derive(Clone)]
pub struct PgAdminPhotoRepository {
    database: Database,
    outbox: PgOutboxRepository,
}

impl PgAdminPhotoRepository {
    pub fn new(database: Database, outbox: PgOutboxRepository) -> Self {
        Self { database, outbox }
    }
}

impl AdminPhotoStore for PgAdminPhotoRepository {
    fn list<'a>(
        &'a self,
        filter: PhotoReconciliationFilter,
        stale_before: DateTime<Utc>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdminPhotoStoreFuture<'a, Vec<AdminPhotoRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT photo.id, photo.user_id, photo.status, photo.size_bytes,
                        photo.width, photo.height, photo.created_at, photo.updated_at,
                        event.status AS outbox_status, event.attempts AS outbox_attempts,
                        event.available_at AS outbox_available_at,
                        event.locked_at AS outbox_locked_at,
                        event.last_error_code AS outbox_last_error_code,
                        CASE
                          WHEN photo.status IN ('pending', 'processing') THEN 'stale_processing'
                          WHEN event.status = 'dead_letter' THEN 'deletion_dead_letter'
                          WHEN event.status = 'processing' THEN 'deletion_processing'
                          WHEN event.status = 'pending' AND event.attempts > 0 THEN 'deletion_retry_scheduled'
                          WHEN event.status = 'pending' THEN 'deletion_queued'
                          WHEN event.status = 'completed' THEN 'deletion_event_completed'
                          WHEN event.status = 'discarded' THEN 'deletion_event_discarded'
                          ELSE 'deletion_event_missing'
                        END AS issue,
                        to_char(photo.updated_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                 FROM user_photo AS photo
                 LEFT JOIN outbox_event AS event
                   ON event.event_type = 'photo.delete' AND event.aggregate_id = photo.id
                 WHERE ((photo.status IN ('pending', 'processing') AND photo.updated_at <= $1)
                         OR photo.status = 'deleting')
                   AND ($2 = 'all'
                     OR ($2 = 'stale_processing' AND photo.status IN ('pending', 'processing'))
                     OR ($2 = 'deleting' AND photo.status = 'deleting')
                     OR ($2 = 'dead_letter' AND photo.status = 'deleting'
                                             AND event.status = 'dead_letter'))
                   AND ($5::timestamptz IS NULL
                     OR (photo.updated_at, photo.id) < ($5::timestamptz, $6::uuid))
                 ORDER BY photo.updated_at DESC, photo.id DESC
                 LIMIT $3 OFFSET $4",
            )
            .bind(stale_before)
            .bind(filter.as_str())
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .bind(cursor.as_ref().map(|value| value.at))
            .bind(cursor.as_ref().map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.iter().map(map_admin_photo_row).collect()
        })
    }

    fn reconcile(
        &self,
        photo_id: Uuid,
        photo_stale_before: DateTime<Utc>,
        outbox_stale_before: DateTime<Utc>,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
    ) -> AdminPhotoStoreFuture<'_, ReconciliationResult> {
        let outbox = self.outbox.clone();
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        type Photo = (Uuid, String, DateTime<Utc>);
                        let photo = sqlx::query_as::<_, Photo>(
                            "SELECT user_id, status, updated_at
                             FROM user_photo WHERE id = $1 FOR UPDATE",
                        )
                        .bind(photo_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some((user_id, status, updated_at)) = photo else {
                            return Ok(ReconciliationResult::NotFound);
                        };
                        if status == "ready"
                            || (status != "deleting" && updated_at > photo_stale_before)
                        {
                            return Ok(ReconciliationResult::NotActionable);
                        }
                        let event = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
                            "SELECT status, locked_at FROM outbox_event
                             WHERE event_type = 'photo.delete' AND aggregate_id = $1
                             FOR UPDATE",
                        )
                        .bind(photo_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if event.is_some_and(|(status, locked_at)| {
                            status == "processing"
                                && locked_at.is_some_and(|at| at > outbox_stale_before)
                        }) {
                            return Ok(ReconciliationResult::AlreadyProcessing);
                        }
                        sqlx::query(
                            "UPDATE user_photo SET status = 'deleting',
                             updated_at = clock_timestamp() WHERE id = $1",
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
                        sqlx::query(
                            "INSERT INTO data_access_log
                             (accessed_user_id, accessor_id, accessor_role, action, reason)
                             VALUES ($1, $2, $3, 'admin_reconcile_photo', $4)",
                        )
                        .bind(user_id)
                        .bind(admin_id)
                        .bind(admin_role.as_str())
                        .bind(reason)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(ReconciliationResult::Queued)
                    })
                })
                .await
        })
    }
}

fn map_admin_photo_row(row: &sqlx::postgres::PgRow) -> Result<AdminPhotoRow, DatabaseError> {
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(map_sqlx_error)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(map_sqlx_error)?;
    let available_at: Option<DateTime<Utc>> =
        row.try_get("outbox_available_at").map_err(map_sqlx_error)?;
    let locked_at: Option<DateTime<Utc>> =
        row.try_get("outbox_locked_at").map_err(map_sqlx_error)?;
    Ok(AdminPhotoRow {
        item: AdminPhotoReconciliation {
            photo_id: row.try_get("id").map_err(map_sqlx_error)?,
            user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
            status: row.try_get("status").map_err(map_sqlx_error)?,
            size_bytes: row.try_get("size_bytes").map_err(map_sqlx_error)?,
            width: row.try_get("width").map_err(map_sqlx_error)?,
            height: row.try_get("height").map_err(map_sqlx_error)?,
            created_at: wire_timestamp(created_at),
            updated_at: wire_timestamp(updated_at),
            outbox_status: row.try_get("outbox_status").map_err(map_sqlx_error)?,
            outbox_attempts: row.try_get("outbox_attempts").map_err(map_sqlx_error)?,
            outbox_available_at: available_at.map(wire_timestamp),
            outbox_locked_at: locked_at.map(wire_timestamp),
            outbox_last_error_code: row
                .try_get("outbox_last_error_code")
                .map_err(map_sqlx_error)?,
            issue: row.try_get("issue").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}
