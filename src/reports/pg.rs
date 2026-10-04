use sqlx::Row as _;
use uuid::Uuid;

use super::domain::{CursorReportRow, PageCursor, ReportReason, ReportRecord, ReportStatus};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

use super::store::{ReportStore, ReportStoreFuture};

#[derive(Clone)]
pub struct PgReportStore {
    database: Database,
}

impl PgReportStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl ReportStore for PgReportStore {
    fn account_exists(&self, user_id: Uuid) -> ReportStoreFuture<'_, bool> {
        Box::pin(async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM user_account
                 WHERE user_id = $1 AND deleted_at IS NULL)",
            )
            .bind(user_id)
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)
        })
    }

    fn match_participants(&self, match_id: Uuid) -> ReportStoreFuture<'_, Option<[Uuid; 2]>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT user1_id, user2_id FROM match_init WHERE id = $1")
                .bind(match_id)
                .fetch_optional(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
            row.map(|row| {
                Ok([
                    row.try_get("user1_id").map_err(map_sqlx_error)?,
                    row.try_get("user2_id").map_err(map_sqlx_error)?,
                ])
            })
            .transpose()
        })
    }

    fn create(&self, report: ReportRecord) -> ReportStoreFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO user_report
                 (id, reporter_id, reported_id, match_id, reason, description,
                  status, created_at, resolved_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(report.id)
            .bind(report.reporter_id)
            .bind(report.reported_id)
            .bind(report.match_id)
            .bind(report.reason.as_str())
            .bind(report.description)
            .bind(report.status.as_str())
            .bind(report.created_at)
            .bind(report.resolved_at)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }

    fn list(
        &self,
        status: Option<ReportStatus>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> ReportStoreFuture<'_, Vec<CursorReportRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, reporter_id, reported_id, match_id, reason, description,
                        status, created_at, resolved_at,
                        to_char(created_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                 FROM user_report
                 WHERE ($1::text IS NULL OR status = $1)
                   AND ($4::timestamptz IS NULL OR
                        (created_at, id) < ($4::timestamptz, $5::uuid))
                 ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
            )
            .bind(status.map(ReportStatus::as_str))
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .bind(cursor.as_ref().map(|value| value.at))
            .bind(cursor.as_ref().map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter().map(map_report_row).collect()
        })
    }

    fn update_status(
        &self,
        id: Uuid,
        status: ReportStatus,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> ReportStoreFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let reported_id: Option<Uuid> = sqlx::query_scalar(
                            "UPDATE user_report
                             SET status = $2,
                                 resolved_at = CASE WHEN $2 = 'pending' THEN NULL
                                   ELSE COALESCE(resolved_at, now()) END
                             WHERE id = $1 RETURNING reported_id",
                        )
                        .bind(id)
                        .bind(status.as_str())
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some(reported_id) = reported_id else {
                            return Ok(false);
                        };
                        sqlx::query(
                            "INSERT INTO data_access_log
                             (accessed_user_id, accessor_id, accessor_role, action, reason)
                             VALUES ($1, $2, $3, 'admin_review_report', $4)",
                        )
                        .bind(reported_id)
                        .bind(admin_id)
                        .bind(admin_role.as_str())
                        .bind(format!("Report {id} moved to {}", status.as_str()))
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(true)
                    })
                })
                .await
        })
    }
}

fn map_report_row(row: sqlx::postgres::PgRow) -> Result<CursorReportRow, DatabaseError> {
    let reason: String = row.try_get("reason").map_err(map_sqlx_error)?;
    let status: String = row.try_get("status").map_err(map_sqlx_error)?;
    Ok(CursorReportRow {
        report: ReportRecord {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            reporter_id: row.try_get("reporter_id").map_err(map_sqlx_error)?,
            reported_id: row.try_get("reported_id").map_err(map_sqlx_error)?,
            match_id: row.try_get("match_id").map_err(map_sqlx_error)?,
            reason: ReportReason::parse(&reason).ok_or(DatabaseError::QueryFailed)?,
            description: row.try_get("description").map_err(map_sqlx_error)?,
            status: ReportStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?,
            created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
            resolved_at: row.try_get("resolved_at").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}
