type ExistingUploadRequest = (
    Vec<u8>,
    String,
    Option<Uuid>,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<String>>,
);

use sqlx::PgConnection;
use uuid::Uuid;

use super::domain::{CreationResult, PhotoObject, ProcessingPhoto};
use super::maintenance::{PhotoMaintenanceFuture, PhotoMaintenanceStore};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::media::codec::ProcessedPhoto;
use crate::moderation::domain::AutomatedPhotoModeration;
use crate::outbox::pg::PgOutboxRepository;
use crate::outbox::types::{NewOutboxEvent, OutboxEventType};
use crate::profiles::domain::{ModerationReason, ModerationStatus};

use super::store::{PhotoStore, PhotoStoreFuture};

#[derive(Clone)]
pub struct PgPhotoRepository {
    database: Database,
    outbox: PgOutboxRepository,
}

impl PgPhotoRepository {
    pub fn new(database: Database, outbox: PgOutboxRepository) -> Self {
        Self { database, outbox }
    }
}

impl PhotoStore for PgPhotoRepository {
    fn create_processing(&self, photo: ProcessingPhoto) -> PhotoStoreFuture<'_, CreationResult> {
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                if !lock_active_profile(connection, photo.user_id).await? {
                    return Ok(CreationResult::ProfileNotFound);
                }
                sqlx::query(
                    "DELETE FROM photo_upload_request
                     WHERE user_id = $1 AND idempotency_key = $2 AND expires_at <= $3",
                )
                .bind(photo.user_id).bind(photo.idempotency_key).bind(photo.created_at)
                .execute(&mut *connection).await.map_err(map_sqlx_error)?;

                let existing = sqlx::query_as::<_, ExistingUploadRequest>(
                    "SELECT request.request_sha256, request.status, request.photo_id,
                            candidate.user_id, candidate.object_key, candidate.status,
                            moderation.status, moderation.reason_codes
                     FROM photo_upload_request AS request
                     LEFT JOIN user_photo AS candidate ON candidate.id = request.photo_id
                     LEFT JOIN content_moderation_case AS moderation ON moderation.photo_id = candidate.id
                     WHERE request.user_id = $1 AND request.idempotency_key = $2",
                )
                .bind(photo.user_id).bind(photo.idempotency_key)
                .fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                if let Some(result) = existing_result(existing, &photo)? { return Ok(result); }

                let active: Option<i32> = sqlx::query_scalar(
                    "SELECT 1 FROM user_photo
                     WHERE user_id = $1 AND status IN ('pending', 'processing') LIMIT 1",
                ).bind(photo.user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                if active.is_some() { return Ok(CreationResult::UpdateInProgress); }

                sqlx::query("INSERT INTO user_photo (id, user_id, object_key, status) VALUES ($1, $2, $3, 'processing')")
                    .bind(photo.id).bind(photo.user_id).bind(&photo.object_key)
                    .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                sqlx::query(
                    "INSERT INTO photo_upload_request
                     (user_id, idempotency_key, request_sha256, photo_id, status, created_at, updated_at, expires_at)
                     VALUES ($1, $2, $3, $4, 'processing', $5, $5, $6)",
                )
                .bind(photo.user_id).bind(photo.idempotency_key).bind(photo.request_sha256.as_slice())
                .bind(photo.id).bind(photo.created_at).bind(photo.expires_at)
                .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(CreationResult::Created)
            })).await
        })
    }

    fn record_processed<'a>(
        &'a self,
        photo_id: Uuid,
        user_id: Uuid,
        photo: &'a ProcessedPhoto,
    ) -> PhotoStoreFuture<'a, bool> {
        let size = i32::try_from(photo.size_bytes);
        let width = i32::try_from(photo.width);
        let height = i32::try_from(photo.height);
        let sha256 = photo.sha256;
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let size = size.map_err(|_| DatabaseError::QueryFailed)?;
                        let width = width.map_err(|_| DatabaseError::QueryFailed)?;
                        let height = height.map_err(|_| DatabaseError::QueryFailed)?;
                        let result = sqlx::query(
                            "UPDATE user_photo SET mime_type = 'image/webp', size_bytes = $3,
                     width = $4, height = $5, sha256 = $6, updated_at = clock_timestamp()
                     WHERE id = $1 AND user_id = $2 AND status = 'processing'",
                        )
                        .bind(photo_id)
                        .bind(user_id)
                        .bind(size)
                        .bind(width)
                        .bind(height)
                        .bind(sha256.as_slice())
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(result.rows_affected() == 1)
                    })
                })
                .await
        })
    }

    fn activate(
        &self,
        photo_id: Uuid,
        user_id: Uuid,
        moderation: AutomatedPhotoModeration,
    ) -> PhotoStoreFuture<'_, bool> {
        let outbox = self.outbox.clone();
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                if !lock_active_profile(connection, user_id).await? { return Ok(false); }
                let candidate: Option<i32> = sqlx::query_scalar(
                    "SELECT 1 FROM user_photo WHERE id = $1 AND user_id = $2 AND status = 'processing'
                     AND mime_type IS NOT NULL AND size_bytes IS NOT NULL AND width IS NOT NULL
                     AND height IS NOT NULL AND sha256 IS NOT NULL FOR UPDATE",
                ).bind(photo_id).bind(user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                if candidate.is_none() { return Ok(false); }

                let previous = sqlx::query_scalar::<_, Uuid>(
                    "UPDATE user_photo SET status = 'deleting', updated_at = clock_timestamp()
                     WHERE user_id = $1 AND status = 'ready' RETURNING id",
                ).bind(user_id).fetch_all(&mut *connection).await.map_err(map_sqlx_error)?;
                for previous_id in previous {
                    outbox.enqueue(connection, &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, previous_id)).await?;
                }
                let activated = sqlx::query(
                    "UPDATE user_photo SET status = 'ready', updated_at = clock_timestamp()
                     WHERE id = $1 AND user_id = $2 AND status = 'processing'",
                ).bind(photo_id).bind(user_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                if activated.rows_affected() != 1 { return Err(DatabaseError::QueryFailed); }
                let reasons = moderation.reasons.iter().map(|reason| reason.as_str()).collect::<Vec<_>>();
                let face_count = moderation.face_count
                    .map(i16::try_from)
                    .transpose()
                    .map_err(|_| DatabaseError::QueryFailed)?;
                sqlx::query(
                    "INSERT INTO content_moderation_case
                     (user_id, content_type, photo_id, status, reason_codes, policy_version,
                      face_count, sharpness_score, nsfw_score)
                     VALUES ($1, 'photo', $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(user_id)
                .bind(photo_id)
                .bind(moderation.status.as_str())
                .bind(reasons)
                .bind(moderation.policy_version)
                .bind(face_count)
                .bind(moderation.sharpness_score)
                .bind(moderation.nsfw_score)
                .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                let request = sqlx::query(
                    "UPDATE photo_upload_request SET status = 'completed', updated_at = clock_timestamp()
                     WHERE user_id = $1 AND photo_id = $2 AND status = 'processing'",
                ).bind(user_id).bind(photo_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                if request.rows_affected() != 1 { return Err(DatabaseError::QueryFailed); }
                Ok(true)
            })).await
        })
    }

    fn begin_delete(&self, user_id: Uuid) -> PhotoStoreFuture<'_, bool> {
        let outbox = self.outbox.clone();
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if !lock_active_profile(connection, user_id).await? {
                            return Ok(false);
                        }
                        let photo_id = sqlx::query_scalar::<_, Uuid>(
                    "UPDATE user_photo SET status = 'deleting', updated_at = clock_timestamp()
                     WHERE user_id = $1 AND status = 'ready' RETURNING id",
                ).bind(user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                        if let Some(photo_id) = photo_id {
                            outbox
                                .enqueue(
                                    connection,
                                    &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, photo_id),
                                )
                                .await?;
                        }
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn begin_account_deletion(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> PhotoStoreFuture<'_, Vec<PhotoObject>> {
        Box::pin(async move {
            let rows = sqlx::query_as::<_, (Uuid, Uuid, String)>(
                "UPDATE user_photo SET status = 'deleting', updated_at = clock_timestamp()
                 WHERE id IN (
                   SELECT id FROM user_photo WHERE user_id = $1 ORDER BY id LIMIT $2
                 )
                 RETURNING id, user_id, object_key",
            )
            .bind(user_id)
            .bind(i64::from(limit))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(rows
                .into_iter()
                .map(|(id, user_id, object_key)| PhotoObject {
                    id,
                    user_id,
                    object_key,
                    moderation_status: ModerationStatus::Pending,
                    moderation_reasons: vec![ModerationReason::AnalysisUnavailable],
                })
                .collect())
        })
    }

    fn find_deleting(&self, photo_id: Uuid) -> PhotoStoreFuture<'_, Option<PhotoObject>> {
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                let row = sqlx::query_as::<_, (Uuid, Uuid, String)>(
                    "SELECT id, user_id, object_key FROM user_photo WHERE id = $1 AND status = 'deleting'",
                ).bind(photo_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(row.map(|(id, user_id, object_key)| PhotoObject {
                    id, user_id, object_key, moderation_status: ModerationStatus::Pending,
                    moderation_reasons: vec![ModerationReason::AnalysisUnavailable],
                }))
            })).await
        })
    }

    fn complete_deletion(&self, photo_id: Uuid) -> PhotoStoreFuture<'_, ()> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        sqlx::query(
                            "UPDATE photo_upload_request SET status = 'consumed', photo_id = NULL,
                     updated_at = clock_timestamp() WHERE photo_id = $1",
                        )
                        .bind(photo_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        sqlx::query("DELETE FROM user_photo WHERE id = $1 AND status = 'deleting'")
                            .bind(photo_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        Ok(())
                    })
                })
                .await
        })
    }

    fn discard_processing(&self, photo_id: Uuid, user_id: Uuid) -> PhotoStoreFuture<'_, ()> {
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                sqlx::query(
                    "DELETE FROM photo_upload_request WHERE user_id = $1 AND photo_id = $2 AND status = 'processing'",
                ).bind(user_id).bind(photo_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                sqlx::query(
                    "DELETE FROM user_photo WHERE id = $1 AND user_id = $2 AND status IN ('pending', 'processing')",
                ).bind(photo_id).bind(user_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(())
            })).await
        })
    }
}

impl PhotoMaintenanceStore for PgPhotoRepository {
    fn purge_expired_upload_requests(
        &self,
        before: chrono::DateTime<chrono::Utc>,
        limit: u32,
    ) -> PhotoMaintenanceFuture<'_, u64> {
        Box::pin(async move {
            let result = sqlx::query(
                "DELETE FROM photo_upload_request
                 WHERE (user_id, idempotency_key) IN (
                   SELECT user_id, idempotency_key FROM photo_upload_request
                   WHERE expires_at <= $1
                   ORDER BY expires_at, user_id, idempotency_key LIMIT $2
                 )",
            )
            .bind(before)
            .bind(i64::from(limit))
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(result.rows_affected())
        })
    }

    fn claim_cleanup_batch(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        stale_before: chrono::DateTime<chrono::Utc>,
        retry_before: chrono::DateTime<chrono::Utc>,
        limit: u32,
    ) -> PhotoMaintenanceFuture<'_, Vec<PhotoObject>> {
        Box::pin(async move {
            let rows = sqlx::query_as::<_, (Uuid, Uuid, String)>(
                "WITH candidates AS (
                   SELECT id FROM user_photo
                   WHERE (status IN ('pending', 'processing') AND updated_at <= $2)
                      OR (status = 'deleting' AND updated_at <= $3)
                   ORDER BY updated_at, id FOR UPDATE SKIP LOCKED LIMIT $4
                 )
                 UPDATE user_photo AS photo
                 SET status = 'deleting', updated_at = $1
                 FROM candidates WHERE photo.id = candidates.id
                 RETURNING photo.id, photo.user_id, photo.object_key",
            )
            .bind(now)
            .bind(stale_before)
            .bind(retry_before)
            .bind(i64::from(limit))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|(id, user_id, object_key)| {
                    Ok(PhotoObject {
                        id,
                        user_id,
                        object_key,
                        moderation_status: ModerationStatus::Pending,
                        moderation_reasons: Vec::new(),
                    })
                })
                .collect()
        })
    }

    fn complete_cleanup(&self, photo_id: Uuid) -> PhotoMaintenanceFuture<'_, ()> {
        self.complete_deletion(photo_id)
    }
}

async fn lock_active_profile(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> Result<bool, DatabaseError> {
    let row: Option<Uuid> = sqlx::query_scalar(
        "SELECT profile.user_id FROM user_profile AS profile
         JOIN user_account AS account ON account.user_id = profile.user_id
         WHERE profile.user_id = $1 AND account.deleted_at IS NULL FOR UPDATE OF profile",
    )
    .bind(user_id)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(row.is_some())
}

fn existing_result(
    row: Option<ExistingUploadRequest>,
    photo: &ProcessingPhoto,
) -> Result<Option<CreationResult>, DatabaseError> {
    let Some((
        hash,
        request_status,
        photo_id,
        user_id,
        object_key,
        photo_status,
        moderation_status,
        reasons,
    )) = row
    else {
        return Ok(None);
    };
    if hash.as_slice() != photo.request_sha256 {
        return Ok(Some(CreationResult::IdempotencyConflict));
    }
    if request_status == "processing" {
        return Ok(Some(CreationResult::UpdateInProgress));
    }
    if request_status == "completed"
        && user_id == Some(photo.user_id)
        && photo_status.as_deref() == Some("ready")
    {
        let status = moderation_status
            .as_deref()
            .and_then(ModerationStatus::parse)
            .unwrap_or(ModerationStatus::Pending);
        let reasons = reasons
            .unwrap_or_else(|| vec!["analysis_unavailable".to_owned()])
            .into_iter()
            .map(|reason| ModerationReason::parse(&reason).ok_or(DatabaseError::QueryFailed))
            .collect::<Result<Vec<_>, _>>()?;
        return match (photo_id, object_key) {
            (Some(id), Some(object_key)) => Ok(Some(CreationResult::Replay(PhotoObject {
                id,
                user_id: photo.user_id,
                object_key,
                moderation_status: status,
                moderation_reasons: reasons,
            }))),
            _ => Ok(Some(CreationResult::IdempotencyConsumed)),
        };
    }
    Ok(Some(CreationResult::IdempotencyConsumed))
}
