//! Refresh rotation and receipt recovery share account, token and family locks.
//! A retry returns the stored active child without moving its session deadline.
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    MobileSessionRepository, RevocationReason, StoredRefreshToken, active_family, insert_token,
    lock_token, revoke_families, same_hash,
};
use crate::{
    identity::mobile::{
        domain::{MobileSessionIdentity, RotationOutcome},
        refresh_recovery::{RefreshRecovery, RefreshRotation},
        tokens::{NewRefreshToken, TokenService},
    },
    infra::postgres::{DatabaseError, map_sqlx_error},
};

impl MobileSessionRepository {
    pub async fn rotate(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
    ) -> Result<RotationOutcome, DatabaseError> {
        Ok(self
            .rotate_inner(jti, hash, next, None)
            .await?
            .into_outcome())
    }

    pub async fn rotate_recoverable(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
        recovery: RefreshRecovery,
    ) -> Result<RefreshRotation, DatabaseError> {
        self.rotate_inner(jti, hash, next, Some(recovery)).await
    }

    async fn rotate_inner(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
        recovery: Option<RefreshRecovery>,
    ) -> Result<RefreshRotation, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let request_hash = recovery.as_ref().map(RefreshRecovery::request_hash);
                    let Some(token) =
                        lock_token(connection, jti, &hash, None, request_hash).await?
                    else {
                        return Ok(RefreshRotation::outcome(RotationOutcome::Invalid));
                    };
                    if !active_family(connection, token.user_id, token.family_id).await? {
                        return Ok(RefreshRotation::outcome(RotationOutcome::Invalid));
                    }
                    if token.revoked {
                        if let Some(receipt) = recovery.as_ref() {
                            let stored_hash: Option<String> = sqlx::query_scalar(
                                "SELECT rotation_request_hash FROM refresh_tokens WHERE id=$1",
                            )
                            .bind(token.id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            if stored_hash
                                .as_deref()
                                .is_some_and(|stored| same_hash(stored, receipt.request_hash()))
                            {
                                return recover_rotation(connection, &token, receipt).await;
                            }
                        }
                        if token.rotated_at.is_some() {
                            revoke_families(
                                connection,
                                token.user_id,
                                RevocationReason::Replay,
                                Some(token.family_id),
                            )
                            .await?;
                            return Ok(RefreshRotation::outcome(RotationOutcome::ReplayRevoked));
                        }
                        return Ok(RefreshRotation::outcome(RotationOutcome::Invalid));
                    }
                    sqlx::query(
                        "UPDATE refresh_tokens
                         SET revoked = true, rotated_at = clock_timestamp(),
                             rotation_request_hash = $2, rotation_result = $3
                         WHERE id = $1",
                    )
                    .bind(token.id)
                    .bind(request_hash)
                    .bind(recovery.as_ref().map(RefreshRecovery::encrypted_token))
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    insert_token(
                        connection,
                        token.user_id,
                        token.family_id,
                        &next,
                        Some(token.id),
                    )
                    .await?;
                    sqlx::query(
                        "UPDATE refresh_token_family
                         SET last_refreshed_at = clock_timestamp(), expires_at = $2
                         WHERE id = $1",
                    )
                    .bind(token.family_id)
                    .bind(next.expires_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(RefreshRotation::outcome(RotationOutcome::Rotated(
                        MobileSessionIdentity {
                            user_id: token.user_id,
                            session_id: token.family_id,
                        },
                    )))
                })
            })
            .await
    }
}

async fn recover_rotation(
    connection: &mut PgConnection,
    parent: &StoredRefreshToken,
    recovery: &RefreshRecovery,
) -> Result<RefreshRotation, DatabaseError> {
    // Account, family and parent locks are already held by rotate_inner. The
    // account lock also serializes child rotation and administrative revocation.
    let row = sqlx::query_as::<_, (Vec<u8>, Uuid, String)>(
        "SELECT parent.rotation_result, child.jti, child.token_hash
         FROM refresh_tokens parent JOIN refresh_tokens child
           ON child.parent_token_id=parent.id AND child.family_id=parent.family_id
         WHERE parent.id=$1 AND parent.rotation_result IS NOT NULL
           AND parent.rotated_at > clock_timestamp() - INTERVAL '7 days'
           AND NOT child.revoked AND child.expires_at > clock_timestamp()",
    )
    .bind(parent.id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let Some((encrypted, child_jti, child_hash)) = row else {
        // A known old attempt is not a new replay. Never revoke a newer session
        // merely because its receipt expired or its child has already rotated.
        return Ok(RefreshRotation::outcome(RotationOutcome::Invalid));
    };
    let raw = recovery
        .open(&encrypted)
        .map_err(|_| DatabaseError::QueryFailed)?;
    let parsed = TokenService::parse_refresh_token(&raw).ok_or(DatabaseError::QueryFailed)?;
    if parsed.jti != child_jti || !same_hash(&child_hash, &parsed.hash) {
        return Err(DatabaseError::QueryFailed);
    }
    Ok(RefreshRotation::recovered(
        MobileSessionIdentity {
            user_id: parent.user_id,
            session_id: parent.family_id,
        },
        raw,
    ))
}
