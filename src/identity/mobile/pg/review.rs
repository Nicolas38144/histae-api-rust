//! Review account creation and session issuance share the account lock and transaction.
use super::{
    MobileSessionIdentity, MobileSessionRepository, NewRefreshToken, insert_session, same_hash,
};
use crate::{
    config::SecretString,
    identity::mobile::play_review::NewReviewAccount,
    infra::postgres::{DatabaseError, map_sqlx_error},
};
use uuid::Uuid;

const REVOKE_CLOSED_REVIEW_SESSIONS: &str = "WITH closed AS (
    UPDATE refresh_token_family family
    SET revoked_at = clock_timestamp(), revocation_reason = 'review_closed'
    FROM user_account account
    WHERE family.user_id = account.user_id AND account.is_review_account
      AND family.revoked_at IS NULL AND (
        $1::text IS NULL OR family.review_cycle_hash IS DISTINCT FROM $1
        OR family.expires_at <= clock_timestamp()
      )
    RETURNING family.id
), refresh AS (
    UPDATE refresh_tokens token SET revoked = true, rotation_result = NULL
    FROM closed WHERE token.family_id = closed.id
), devices AS (
    DELETE FROM device_token device USING closed WHERE device.session_id = closed.id
)
SELECT count(*) FROM closed";

impl MobileSessionRepository {
    pub async fn revoke_closed_review_sessions(&self) -> Result<i64, DatabaseError> {
        let cycle_hash = self.review_cycle_hash.clone();
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    // Use the account lock order shared with refresh rotation.
                    sqlx::query(
                        "SELECT user_id FROM user_account
                         WHERE is_review_account ORDER BY user_id FOR UPDATE",
                    )
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    sqlx::query_scalar(REVOKE_CLOSED_REVIEW_SESSIONS)
                        .bind(cycle_hash.as_ref().map(SecretString::expose_secret))
                        .fetch_one(connection)
                        .await
                        .map_err(map_sqlx_error)
                })
            })
            .await
    }

    pub(super) async fn create_review_session(
        &self,
        account: NewReviewAccount,
        mut token: NewRefreshToken,
    ) -> Result<Option<MobileSessionIdentity>, DatabaseError> {
        if !self
            .review_cycle_hash()
            .is_some_and(|cycle| same_hash(cycle, account.cycle_hash.expose_secret()))
        {
            return Ok(None);
        }
        token.expires_at = token.expires_at.min(account.expires_at);
        if token.expires_at <= token.created_at {
            return Ok(None);
        }
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
                    // First creation requires the latest unexpired signup intent.
                    // A login request, or verification alone, must never register an account.
                    let registering: Option<bool> = sqlx::query_scalar(
                        "SELECT registration AND expires_at > clock_timestamp()
                         FROM mobile_otp_request WHERE phone_hash=$1
                         ORDER BY created_at DESC, id DESC LIMIT 1",
                    )
                    .bind(&account.phone_hash)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if registering == Some(true) {
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
                    }
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
                    insert_session(
                        connection,
                        user_id,
                        &token,
                        Some(account.cycle_hash.expose_secret()),
                    )
                    .await
                    .map(Some)
                })
            })
            .await;
        match result {
            Err(DatabaseError::AccountUnavailable) => Ok(None),
            result => result,
        }
    }
}
