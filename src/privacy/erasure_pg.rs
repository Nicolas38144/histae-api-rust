use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use super::erasure::{
    AcceptedErasure, AcceptedErasureStatus, AccountDeletionFuture, AccountDeletionStore,
    ClaimedErasure, ErasureStep, ErasureStore, ErasureStoreFuture, NewDeletionToken,
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

#[derive(Clone)]
pub struct PgErasureRepository {
    database: Database,
}

impl PgErasureRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl AccountDeletionStore for PgErasureRepository {
    fn replace_token(&self, token: NewDeletionToken) -> AccountDeletionFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let account = sqlx::query_scalar::<_, Uuid>(
                            "SELECT user_id FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL FOR UPDATE",
                        )
                        .bind(token.user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if account.is_none() {
                            return Ok(false);
                        }
                        sqlx::query("DELETE FROM account_deletion_token WHERE user_id = $1")
                            .bind(token.user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        sqlx::query(
                            "INSERT INTO account_deletion_token
                               (id, user_id, token_hash, expires_at)
                             VALUES ($1, $2, $3, $4)",
                        )
                        .bind(token.id)
                        .bind(token.user_id)
                        .bind(token.token_hash)
                        .bind(token.expires_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn accept(
        &self,
        user_id: Uuid,
        token_id: Uuid,
        token_hash: String,
        now: chrono::DateTime<chrono::Utc>,
    ) -> AccountDeletionFuture<'_, Option<AcceptedErasure>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let account = sqlx::query_scalar::<_, Uuid>(
                            "SELECT user_id FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL FOR UPDATE",
                        )
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if account.is_none() {
                            return Ok(None);
                        }
                        let consumed = sqlx::query(
                            "DELETE FROM account_deletion_token
                             WHERE id = $1 AND user_id = $2 AND token_hash = $3
                               AND expires_at > $4",
                        )
                        .bind(token_id)
                        .bind(user_id)
                        .bind(token_hash)
                        .bind(now)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if consumed.rows_affected() != 1 {
                            return Ok(None);
                        }
                        enqueue_account_erasure(connection, user_id, None)
                            .await
                            .map(Some)
                    })
                })
                .await
        })
    }
}

impl ErasureStore for PgErasureRepository {
    fn claimed(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
    ) -> ErasureStoreFuture<'_, Option<ClaimedErasure>> {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT erasure.request_id, erasure.user_id, erasure.step
                 FROM account_erasure AS erasure
                 JOIN outbox_event AS event ON event.aggregate_id = erasure.request_id
                 WHERE event.id = $1 AND event.event_type = 'account.erase'
                   AND event.status = 'processing' AND event.locked_by = $2",
            )
            .bind(event_id)
            .bind(worker_id)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                let step = row.try_get::<String, _>("step").map_err(map_sqlx_error)?;
                Ok(ClaimedErasure {
                    request_id: row.try_get("request_id").map_err(map_sqlx_error)?,
                    user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                    step: ErasureStep::parse(&step).ok_or(DatabaseError::QueryFailed)?,
                })
            })
            .transpose()
        })
    }

    fn advance(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
        current: ClaimedErasure,
        next: ErasureStep,
    ) -> ErasureStoreFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if !lock_owned_event(connection, event_id, worker_id).await? {
                            return Ok(false);
                        }
                        let expected = sqlx::query_scalar::<_, Uuid>(
                            "SELECT request_id FROM account_erasure
                             WHERE request_id = $1 AND step = $2 FOR UPDATE",
                        )
                        .bind(current.request_id)
                        .bind(current.step.as_str())
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if expected.is_none() {
                            return Ok(false);
                        }

                        if next == ErasureStep::Completed {
                            if current.step != ErasureStep::Postgres {
                                return Err(DatabaseError::QueryFailed);
                            }
                            sqlx::query(
                                "SELECT id FROM match_init
                                 WHERE user1_id = $1 OR user2_id = $1
                                 ORDER BY id FOR UPDATE",
                            )
                            .bind(current.user_id)
                            .fetch_all(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            sqlx::query(
                                "SELECT user_id FROM user_account WHERE user_id = $1 FOR UPDATE",
                            )
                            .bind(current.user_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            let request = sqlx::query_scalar::<_, Uuid>(
                                "SELECT id FROM data_subject_request
                                 WHERE id = $1 AND user_id = $2 AND status = 'in_progress'
                                   AND type = 'erasure' FOR UPDATE",
                            )
                            .bind(current.request_id)
                            .bind(current.user_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            if request.is_none() {
                                return Err(DatabaseError::QueryFailed);
                            }
                            let photos = sqlx::query_scalar::<_, i32>(
                                "SELECT 1 FROM user_photo WHERE user_id = $1 LIMIT 1",
                            )
                            .bind(current.user_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            if photos.is_some() {
                                return Err(DatabaseError::QueryFailed);
                            }
                            let swipes = sqlx::query_scalar::<_, i32>(
                                "SELECT 1 FROM swipe_decision
                                 WHERE actor_id = $1 OR target_id = $1 LIMIT 1",
                            )
                            .bind(current.user_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            if swipes.is_some() {
                                return Err(DatabaseError::QueryFailed);
                            }
                            sqlx::query("DELETE FROM photo_upload_request WHERE user_id = $1")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query("DELETE FROM admin_session WHERE user_id = $1")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query("DELETE FROM admin_webauthn_challenge WHERE user_id = $1")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query("DELETE FROM admin_webauthn_bootstrap WHERE user_id = $1")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query("DELETE FROM admin_webauthn_credential WHERE user_id = $1")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query(
                                "DELETE FROM outbox_event
                                 WHERE event_type = 'billing.subscription.reconcile'
                                   AND aggregate_id = $1",
                            )
                            .bind(current.user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            sqlx::query(
                                "DELETE FROM outbox_event
                                 WHERE event_type = 'billing.customer.reconcile'
                                   AND aggregate_id IN (
                                     SELECT id FROM billing_checkout_session WHERE user_id = $1
                                   )",
                            )
                            .bind(current.user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            sqlx::query("SELECT fct_anonymize_user($1)")
                                .bind(current.user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            sqlx::query(
                                "UPDATE data_subject_request
                                 SET status = 'completed', completed_at = clock_timestamp()
                                 WHERE id = $1 AND status = 'in_progress'",
                            )
                            .bind(current.request_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }

                        sqlx::query(
                            "UPDATE account_erasure SET step = $2,
                               updated_at = clock_timestamp(),
                               completed_at = CASE WHEN $2 = 'completed'
                                 THEN clock_timestamp() END
                             WHERE request_id = $1 AND step = $3",
                        )
                        .bind(current.request_id)
                        .bind(next.as_str())
                        .bind(current.step.as_str())
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if next != ErasureStep::Completed {
                            release_for_next_batch(connection, event_id, worker_id, 1).await?;
                        }
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn delete_swipe_batch(
        &self,
        event_id: Uuid,
        worker_id: Uuid,
        current: ClaimedErasure,
        batch_size: u32,
    ) -> ErasureStoreFuture<'_, bool> {
        Box::pin(async move {
            if current.step != ErasureStep::Swipes {
                return Err(DatabaseError::QueryFailed);
            }
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        if !lock_owned_event(connection, event_id, worker_id).await? {
                            return Ok(false);
                        }
                        let expected = sqlx::query_scalar::<_, Uuid>(
                            "SELECT request_id FROM account_erasure
                             WHERE request_id = $1 AND step = 'swipes' FOR UPDATE",
                        )
                        .bind(current.request_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if expected.is_none() {
                            return Ok(false);
                        }
                        let limit = i64::from(batch_size);
                        let deleted = sqlx::query(
                            "DELETE FROM swipe_decision AS swipe
                             USING (
                               SELECT actor_id, target_id FROM (
                                 (SELECT actor_id, target_id FROM swipe_decision
                                  WHERE actor_id = $1 ORDER BY target_id LIMIT $2)
                                 UNION ALL
                                 (SELECT actor_id, target_id FROM swipe_decision
                                  WHERE target_id = $1 ORDER BY actor_id LIMIT $2)
                               ) AS candidates LIMIT $2
                             ) AS doomed
                             WHERE swipe.actor_id = doomed.actor_id
                               AND swipe.target_id = doomed.target_id",
                        )
                        .bind(current.user_id)
                        .bind(limit)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let next = if deleted.rows_affected() < u64::from(batch_size) {
                            ErasureStep::Postgres
                        } else {
                            ErasureStep::Swipes
                        };
                        sqlx::query(
                            "UPDATE account_erasure SET step = $2,
                               updated_at = clock_timestamp()
                             WHERE request_id = $1 AND step = 'swipes'",
                        )
                        .bind(current.request_id)
                        .bind(next.as_str())
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        release_for_next_batch(connection, event_id, worker_id, 1).await?;
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn defer(&self, event_id: Uuid, worker_id: Uuid) -> ErasureStoreFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query(
                "UPDATE outbox_event SET status = 'pending',
                   attempts = GREATEST(0, attempts - 1),
                   available_at = clock_timestamp() + interval '5 seconds',
                   locked_at = NULL, locked_by = NULL
                 WHERE id = $1 AND status = 'processing' AND locked_by = $2",
            )
            .bind(event_id)
            .bind(worker_id)
            .execute(self.database.pool())
            .await
            .map(|_| ())
            .map_err(map_sqlx_error)
        })
    }
}

pub(crate) async fn enqueue_account_erasure(
    connection: &mut PgConnection,
    user_id: Uuid,
    supplied_request_id: Option<Uuid>,
) -> Result<AcceptedErasure, DatabaseError> {
    let existing =
        sqlx::query_scalar::<_, Uuid>("SELECT request_id FROM account_erasure WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    if let Some(request_id) = existing {
        if supplied_request_id.is_some_and(|supplied| supplied != request_id) {
            return Err(DatabaseError::QueryFailed);
        }
        return Ok(AcceptedErasure {
            request_id,
            status: AcceptedErasureStatus::InProgress,
        });
    }

    let request_id = match supplied_request_id {
        Some(request_id) => request_id,
        None => sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO data_subject_request (user_id, type, status)
             VALUES ($1, 'erasure', 'in_progress')
             ON CONFLICT (user_id, type)
               WHERE status IN ('pending', 'in_progress')
             DO UPDATE SET status = 'in_progress'
             RETURNING id",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?,
    };
    sqlx::query("INSERT INTO account_erasure (request_id, user_id) VALUES ($1, $2)")
        .bind(request_id)
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO outbox_event (id, event_type, aggregate_id)
         VALUES ($1, 'account.erase', $2)
         ON CONFLICT (event_type, aggregate_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(request_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE user_account SET deleted_at = COALESCE(deleted_at, clock_timestamp())
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM account_deletion_token WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(AcceptedErasure {
        request_id,
        status: AcceptedErasureStatus::InProgress,
    })
}

async fn lock_owned_event(
    connection: &mut PgConnection,
    event_id: Uuid,
    worker_id: Uuid,
) -> Result<bool, DatabaseError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM outbox_event
         WHERE id = $1 AND status = 'processing' AND locked_by = $2 FOR UPDATE",
    )
    .bind(event_id)
    .bind(worker_id)
    .fetch_optional(connection)
    .await
    .map(|row| row.is_some())
    .map_err(map_sqlx_error)
}

async fn release_for_next_batch(
    connection: &mut PgConnection,
    event_id: Uuid,
    worker_id: Uuid,
    delay_seconds: i32,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "UPDATE outbox_event SET status = 'pending', attempts = 0,
           available_at = clock_timestamp() + make_interval(secs => $3),
           locked_at = NULL, locked_by = NULL, last_error_code = NULL
         WHERE id = $1 AND locked_by = $2",
    )
    .bind(event_id)
    .bind(worker_id)
    .bind(delay_seconds)
    .execute(connection)
    .await
    .map(|_| ())
    .map_err(map_sqlx_error)
}
