use std::future::Future;
use std::pin::Pin;

use sqlx::Row as _;
use uuid::Uuid;

use super::domain::BlockedUserRow;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

const ENDED_MATCH_RETENTION_DAYS: i32 = 30;

pub type PrivacyStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait PrivacyStore: Send + Sync {
    fn block(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, bool>;
    fn unblock(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, ()>;
    fn blocked_users(&self, blocker_id: Uuid) -> PrivacyStoreFuture<'_, Vec<BlockedUserRow>>;
}

#[derive(Clone)]
pub struct PgPrivacyStore {
    database: Database,
}

impl PgPrivacyStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl PrivacyStore for PgPrivacyStore {
    fn block(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let target: Option<Uuid> = sqlx::query_scalar(
                            "SELECT user_id FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL",
                        )
                        .bind(blocked_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if target.is_none() {
                            return Ok(false);
                        }
                        sqlx::query(
                            "INSERT INTO user_block (blocker_id, blocked_id)
                             VALUES ($1, $2) ON CONFLICT DO NOTHING",
                        )
                        .bind(blocker_id)
                        .bind(blocked_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        sqlx::query(
                            "UPDATE match_init
                             SET status = 'ended',
                                 purge_after = clock_timestamp() + ($3 * INTERVAL '1 day')
                             WHERE ((user1_id = $1 AND user2_id = $2)
                                 OR (user1_id = $2 AND user2_id = $1))
                               AND status IN ('active', 'awaiting_continuation', 'confirmed')",
                        )
                        .bind(blocker_id)
                        .bind(blocked_id)
                        .bind(ENDED_MATCH_RETENTION_DAYS)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(true)
                    })
                })
                .await
        })
    }

    fn unblock(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query("DELETE FROM user_block WHERE blocker_id = $1 AND blocked_id = $2")
                .bind(blocker_id)
                .bind(blocked_id)
                .execute(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
            Ok(())
        })
    }

    fn blocked_users(&self, blocker_id: Uuid) -> PrivacyStoreFuture<'_, Vec<BlockedUserRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT block.blocked_id AS user_id, profile.firstname,
                        block.created_at AS blocked_at
                 FROM user_block AS block
                 LEFT JOIN user_profile AS profile ON profile.user_id = block.blocked_id
                   AND EXISTS (SELECT 1 FROM user_account
                               WHERE user_id = block.blocked_id AND deleted_at IS NULL)
                 WHERE block.blocker_id = $1
                 ORDER BY block.created_at DESC",
            )
            .bind(blocker_id)
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|row| {
                    Ok(BlockedUserRow {
                        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                        firstname: row.try_get("firstname").map_err(map_sqlx_error)?,
                        blocked_at: row.try_get("blocked_at").map_err(map_sqlx_error)?,
                    })
                })
                .collect()
        })
    }
}
