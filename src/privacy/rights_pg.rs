use chrono::{DateTime, Utc};
use sqlx::Row as _;
use uuid::Uuid;

use super::erasure_pg::enqueue_account_erasure;
use super::rights::{
    AdminDataRequestRow, DataAccessLogRow, DataRequestRow, DataRequestStatus,
    DataRequestTransition, DataRequestType, DataRightsFuture, DataRightsStore, ErasureProgress,
    PageCursor, UpdateRequestInput, UpdateRequestResult,
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

#[derive(Clone)]
pub struct PgDataRightsStore {
    database: Database,
}

impl PgDataRightsStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl DataRightsStore for PgDataRightsStore {
    fn create_request(
        &self,
        user_id: Uuid,
        request_type: DataRequestType,
    ) -> DataRightsFuture<'_, Option<DataRequestRow>> {
        Box::pin(async move {
            let row = sqlx::query(
                "INSERT INTO data_subject_request (user_id, type)
                 VALUES ($1, $2)
                 ON CONFLICT (user_id, type)
                   WHERE status IN ('pending', 'in_progress') DO NOTHING
                 RETURNING id, user_id, type, status, requested_at, completed_at, handled_by",
            )
            .bind(user_id)
            .bind(request_type.as_str())
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            row.map(map_request_row).transpose()
        })
    }

    fn requests_for_user(&self, user_id: Uuid) -> DataRightsFuture<'_, Vec<DataRequestRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, user_id, type, status, requested_at, completed_at, handled_by
                 FROM data_subject_request WHERE user_id = $1
                 ORDER BY requested_at DESC, id DESC",
            )
            .bind(user_id)
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter().map(map_request_row).collect()
        })
    }

    fn requests_for_admin(
        &self,
        status: Option<DataRequestStatus>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<AdminDataRequestRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT request.id, request.user_id, request.type, request.status,
                        request.requested_at, request.completed_at, request.handled_by,
                        request.notes,
                        to_char(request.requested_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
                        erasure.request_id AS erasure_request_id, erasure.step AS erasure_step,
                        erasure.updated_at AS erasure_updated_at,
                        event.id AS event_id, event.status AS event_status,
                        COALESCE(event.attempts, 0) AS event_attempts,
                        event.last_error_code
                 FROM data_subject_request AS request
                 LEFT JOIN account_erasure AS erasure ON erasure.request_id = request.id
                 LEFT JOIN outbox_event AS event
                   ON event.aggregate_id = request.id AND event.event_type = 'account.erase'
                 WHERE ($1::text IS NULL OR request.status = $1)
                   AND ($4::timestamptz IS NULL
                     OR (request.requested_at, request.id) < ($4::timestamptz, $5::uuid))
                 ORDER BY request.requested_at DESC, request.id DESC
                 LIMIT $2 OFFSET $3",
            )
            .bind(status.map(DataRequestStatus::as_str))
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .bind(cursor.as_ref().map(|value| value.at))
            .bind(cursor.map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter().map(map_admin_request_row).collect()
        })
    }

    fn update_request(
        &self,
        input: UpdateRequestInput,
    ) -> DataRightsFuture<'_, UpdateRequestResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let owner = sqlx::query_scalar::<_, Uuid>(
                            "SELECT user_id FROM data_subject_request WHERE id = $1",
                        )
                        .bind(input.request_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some(user_id) = owner else {
                            return Ok(UpdateRequestResult::NotFound);
                        };

                        sqlx::query(
                            "SELECT user_id FROM user_account WHERE user_id = $1 FOR UPDATE",
                        )
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;

                        let request = sqlx::query(
                            "SELECT id, user_id, type, status, requested_at, completed_at,
                                    handled_by, notes
                             FROM data_subject_request WHERE id = $1 FOR UPDATE",
                        )
                        .bind(input.request_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some(request) = request else {
                            return Ok(UpdateRequestResult::NotFound);
                        };
                        let request_type = request
                            .try_get::<String, _>("type")
                            .map_err(map_sqlx_error)
                            .and_then(|value| {
                                DataRequestType::parse(&value).ok_or(DatabaseError::QueryFailed)
                            })?;
                        let current_status = request
                            .try_get::<String, _>("status")
                            .map_err(map_sqlx_error)
                            .and_then(|value| {
                                DataRequestStatus::parse(&value).ok_or(DatabaseError::QueryFailed)
                            })?;

                        let workflow = sqlx::query_scalar::<_, Uuid>(
                            "SELECT request_id FROM account_erasure WHERE request_id = $1",
                        )
                        .bind(input.request_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if workflow.is_some() {
                            return Ok(
                                if current_status == DataRequestStatus::InProgress
                                    && input.status == DataRequestTransition::Completed
                                {
                                    UpdateRequestResult::ErasureScheduled
                                } else {
                                    UpdateRequestResult::InvalidTransition
                                },
                            );
                        }

                        let requested_status = input.status.as_status();
                        let allowed = match current_status {
                            DataRequestStatus::Pending => matches!(
                                requested_status,
                                DataRequestStatus::InProgress | DataRequestStatus::Rejected
                            ),
                            DataRequestStatus::InProgress => matches!(
                                requested_status,
                                DataRequestStatus::Completed | DataRequestStatus::Rejected
                            ),
                            DataRequestStatus::Completed | DataRequestStatus::Rejected => false,
                        };
                        if !allowed {
                            return Ok(UpdateRequestResult::InvalidTransition);
                        }

                        let scheduling = request_type == DataRequestType::Erasure
                            && input.status == DataRequestTransition::Completed;
                        let stored_status = if scheduling {
                            DataRequestStatus::InProgress
                        } else {
                            requested_status
                        };
                        sqlx::query(
                            "UPDATE data_subject_request
                             SET status = $2, handled_by = $3, notes = $4,
                               completed_at = CASE WHEN $2 IN ('completed', 'rejected')
                                 THEN clock_timestamp() ELSE NULL END
                             WHERE id = $1",
                        )
                        .bind(input.request_id)
                        .bind(stored_status.as_str())
                        .bind(input.admin_id)
                        .bind(input.notes)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;

                        let reason = if scheduling {
                            "DSR erasure scheduled".to_owned()
                        } else {
                            format!(
                                "DSR {} moved to {}",
                                request_type.as_str(),
                                requested_status.as_str()
                            )
                        };
                        sqlx::query(
                            "INSERT INTO data_access_log
                               (accessed_user_id, accessor_id, accessor_role, action, reason)
                             VALUES ($1, $2, $3, 'admin_review_dsr', $4)",
                        )
                        .bind(user_id)
                        .bind(input.admin_id)
                        .bind(input.admin_role.as_str())
                        .bind(reason)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;

                        if scheduling {
                            enqueue_account_erasure(connection, user_id, Some(input.request_id))
                                .await?;
                            return Ok(UpdateRequestResult::ErasureScheduled);
                        }

                        Ok(UpdateRequestResult::Updated)
                    })
                })
                .await
        })
    }

    fn access_logs(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<DataAccessLogRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, accessed_user_id, accessor_id, accessor_role, action, reason,
                        accessed_at,
                        to_char(accessed_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                 FROM data_access_log WHERE accessed_user_id = $1
                   AND ($4::timestamptz IS NULL
                     OR (accessed_at, id) < ($4::timestamptz, $5::uuid))
                 ORDER BY accessed_at DESC, id DESC LIMIT $2 OFFSET $3",
            )
            .bind(user_id)
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .bind(cursor.as_ref().map(|value| value.at))
            .bind(cursor.map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter().map(map_access_log_row).collect()
        })
    }

    fn record_self_export(&self, user_id: Uuid) -> DataRightsFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO data_access_log
                   (accessed_user_id, accessor_id, accessor_role, action, reason)
                 VALUES ($1, $1, 'user', 'export_data', 'Self-service data export')",
            )
            .bind(user_id)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }
}

fn map_request_row(row: sqlx::postgres::PgRow) -> Result<DataRequestRow, DatabaseError> {
    let request_type = row
        .try_get::<String, _>("type")
        .map_err(map_sqlx_error)
        .and_then(|value| DataRequestType::parse(&value).ok_or(DatabaseError::QueryFailed))?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(map_sqlx_error)
        .and_then(|value| DataRequestStatus::parse(&value).ok_or(DatabaseError::QueryFailed))?;
    Ok(DataRequestRow {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
        request_type,
        status,
        requested_at: row.try_get("requested_at").map_err(map_sqlx_error)?,
        completed_at: row.try_get("completed_at").map_err(map_sqlx_error)?,
        handled_by: row.try_get("handled_by").map_err(map_sqlx_error)?,
    })
}

fn map_admin_request_row(row: sqlx::postgres::PgRow) -> Result<AdminDataRequestRow, DatabaseError> {
    let erasure_request_id: Option<Uuid> =
        row.try_get("erasure_request_id").map_err(map_sqlx_error)?;
    let erasure = if erasure_request_id.is_some() {
        let attempts: i16 = row.try_get("event_attempts").map_err(map_sqlx_error)?;
        Some(ErasureProgress {
            step: row.try_get("erasure_step").map_err(map_sqlx_error)?,
            updated_at: wire_timestamp(row.try_get("erasure_updated_at").map_err(map_sqlx_error)?),
            event_id: row.try_get("event_id").map_err(map_sqlx_error)?,
            status: row.try_get("event_status").map_err(map_sqlx_error)?,
            attempts: u16::try_from(attempts).map_err(|_| DatabaseError::QueryFailed)?,
            last_error_code: row.try_get("last_error_code").map_err(map_sqlx_error)?,
        })
    } else {
        None
    };
    let request_type = row
        .try_get::<String, _>("type")
        .map_err(map_sqlx_error)
        .and_then(|value| DataRequestType::parse(&value).ok_or(DatabaseError::QueryFailed))?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(map_sqlx_error)
        .and_then(|value| DataRequestStatus::parse(&value).ok_or(DatabaseError::QueryFailed))?;
    Ok(AdminDataRequestRow {
        request: DataRequestRow {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
            request_type,
            status,
            requested_at: row.try_get("requested_at").map_err(map_sqlx_error)?,
            completed_at: row.try_get("completed_at").map_err(map_sqlx_error)?,
            handled_by: row.try_get("handled_by").map_err(map_sqlx_error)?,
        },
        notes: row.try_get("notes").map_err(map_sqlx_error)?,
        erasure,
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn map_access_log_row(row: sqlx::postgres::PgRow) -> Result<DataAccessLogRow, DatabaseError> {
    Ok(DataAccessLogRow {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        accessed_user_id: row.try_get("accessed_user_id").map_err(map_sqlx_error)?,
        accessor_id: row.try_get("accessor_id").map_err(map_sqlx_error)?,
        accessor_role: row.try_get("accessor_role").map_err(map_sqlx_error)?,
        action: row.try_get("action").map_err(map_sqlx_error)?,
        reason: row.try_get("reason").map_err(map_sqlx_error)?,
        accessed_at: row.try_get("accessed_at").map_err(map_sqlx_error)?,
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
