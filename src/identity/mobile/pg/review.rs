//! Review account creation and session issuance share the account lock and transaction.
use super::{MobileSessionIdentity, MobileSessionRepository, NewRefreshToken, insert_session};
use crate::{
    identity::mobile::play_review::NewReviewAccount,
    infra::postgres::{DatabaseError, map_sqlx_error},
};
use uuid::Uuid;

impl MobileSessionRepository {
    pub(super) async fn create_review_session(
        &self,
        account: NewReviewAccount,
        token: NewRefreshToken,
    ) -> Result<Option<MobileSessionIdentity>, DatabaseError> {
        let result = self
            .database
            .transaction(|connection| {
                Box::pin(async move {
                    // Serialize creation for this phone, including concurrent reviewer logins.
                    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 6837822))")
                        .bind(&account.phone_hash)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    let tombstone: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM account_tombstone
                         WHERE phone_number_hash=$1 AND expires_at>clock_timestamp())",
                    )
                    .bind(&account.phone_hash)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if tombstone {
                        return Ok(None);
                    }
                    sqlx::query(
                        "INSERT INTO user_account
                         (user_id,phone_number_hash,phone_number_encrypted,is_review_account)
                         VALUES($1,$2,$3,true) ON CONFLICT(phone_number_hash) DO NOTHING",
                    )
                    .bind(account.user_id)
                    .bind(&account.phone_hash)
                    .bind(account.encrypted_phone)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    // An account deletion can commit a tombstone while this INSERT
                    // waits on the unique phone key. Recheck on a fresh snapshot and
                    // roll back a newly inserted account when that happened.
                    let blocked: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM account_tombstone
                         WHERE phone_number_hash=$1 AND expires_at>clock_timestamp())",
                    )
                    .bind(&account.phone_hash)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if blocked {
                        return Err(DatabaseError::AccountUnavailable);
                    }
                    // Never adopt a real account, revive a deletion or ignore a moderation ban.
                    let user_id: Option<Uuid> = sqlx::query_scalar(
                        "SELECT user_id FROM user_account WHERE phone_number_hash=$1
                         AND is_review_account AND role='user' AND NOT is_banned
                         AND deleted_at IS NULL FOR UPDATE",
                    )
                    .bind(&account.phone_hash)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some(user_id) = user_id else {
                        return Ok(None);
                    };
                    insert_session(connection, user_id, &token).await.map(Some)
                })
            })
            .await;
        match result {
            Err(DatabaseError::AccountUnavailable) => Ok(None),
            result => result,
        }
    }
}
