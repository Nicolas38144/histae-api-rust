use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

use super::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use super::tokens::NewRefreshToken;

pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait MobileSessionStore: Send + Sync {
    fn create(
        &self,
        user_id: Uuid,
        token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>>;

    fn rotate(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome>;

    fn logout(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        jti: Uuid,
        hash: String,
        device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool>;

    fn list(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>>;

    fn revoke(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>>;

    fn active_account(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        terms_version: Arc<str>,
        privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>>;
}

#[derive(Clone)]
pub struct MobileSessionRepository {
    database: Database,
}

impl MobileSessionRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        token: NewRefreshToken,
    ) -> Result<Option<MobileSessionIdentity>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    if !lock_active_account(connection, user_id).await? {
                        return Ok(None);
                    }
                    let session_id = random_uuid_v4();
                    sqlx::query(
                        "INSERT INTO refresh_token_family
                         (id, user_id, created_at, last_refreshed_at, expires_at)
                         VALUES ($1, $2, $3, $3, $4)",
                    )
                    .bind(session_id)
                    .bind(user_id)
                    .bind(token.created_at)
                    .bind(token.expires_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    insert_token(connection, user_id, session_id, &token, None).await?;
                    Ok(Some(MobileSessionIdentity {
                        user_id,
                        session_id,
                    }))
                })
            })
            .await
    }

    pub async fn rotate(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
    ) -> Result<RotationOutcome, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let Some(token) = lock_token(connection, jti, &hash, None).await? else {
                        return Ok(RotationOutcome::Invalid);
                    };
                    if !active_family(connection, token.user_id, token.family_id).await? {
                        return Ok(RotationOutcome::Invalid);
                    }
                    if token.revoked {
                        if token.rotated_at.is_some() {
                            revoke_families(
                                connection,
                                token.user_id,
                                RevocationReason::Replay,
                                Some(token.family_id),
                            )
                            .await?;
                            return Ok(RotationOutcome::ReplayRevoked);
                        }
                        return Ok(RotationOutcome::Invalid);
                    }
                    sqlx::query(
                        "UPDATE refresh_tokens
                         SET revoked = true, rotated_at = clock_timestamp()
                         WHERE id = $1",
                    )
                    .bind(token.id)
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
                    Ok(RotationOutcome::Rotated(MobileSessionIdentity {
                        user_id: token.user_id,
                        session_id: token.family_id,
                    }))
                })
            })
            .await
    }

    pub async fn logout(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        jti: Uuid,
        hash: String,
        device_id: Option<Uuid>,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let Some(token) = lock_token(connection, jti, &hash, Some(user_id)).await?
                    else {
                        return Ok(false);
                    };
                    if token.family_id != session_id
                        || !active_family(connection, user_id, session_id).await?
                    {
                        return Ok(false);
                    }
                    revoke_families(
                        connection,
                        user_id,
                        RevocationReason::Logout,
                        Some(session_id),
                    )
                    .await?;
                    if let Some(device_id) = device_id {
                        sqlx::query("DELETE FROM device_token WHERE id = $1 AND user_id = $2")
                            .bind(device_id)
                            .bind(user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                    }
                    Ok(true)
                })
            })
            .await
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<&SessionCursor>,
    ) -> Result<Vec<MobileSessionRow>, DatabaseError> {
        let rows = sqlx::query_as::<_, (Uuid, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>, String)>(
            "SELECT id, created_at, last_refreshed_at, expires_at,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
             FROM refresh_token_family
             WHERE user_id = $1 AND revoked_at IS NULL
               AND expires_at > statement_timestamp()
               AND ($3::timestamptz IS NULL OR (created_at, id) < ($3::timestamptz, $4::uuid))
             ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(i64::from(limit))
        .bind(cursor.map(|cursor| cursor.at.as_str()))
        .bind(cursor.map(|cursor| cursor.id))
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(
                |(id, created_at, last_refreshed_at, expires_at, cursor_at)| MobileSessionRow {
                    id,
                    created_at,
                    last_refreshed_at,
                    expires_at,
                    cursor_at,
                },
            )
            .collect())
    }

    pub async fn revoke(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        target_id: Option<Uuid>,
    ) -> Result<Option<u64>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    if !lock_active_account(connection, user_id).await?
                        || !active_family(connection, user_id, current_session_id).await?
                    {
                        return Ok(None);
                    }
                    if let Some(target_id) = target_id {
                        let target = sqlx::query_scalar::<_, Uuid>(
                            "SELECT id FROM refresh_token_family
                             WHERE id = $1 AND user_id = $2",
                        )
                        .bind(target_id)
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if target.is_none() {
                            return Ok(Some(0));
                        }
                    }
                    let count = revoke_families(
                        connection,
                        user_id,
                        if target_id.is_some() {
                            RevocationReason::UserRevoked
                        } else {
                            RevocationReason::LogoutAll
                        },
                        target_id,
                    )
                    .await?;
                    Ok(Some(if target_id.is_some() { 1 } else { count }))
                })
            })
            .await
    }

    pub async fn is_active(&self, user_id: Uuid, session_id: Uuid) -> Result<bool, DatabaseError> {
        let active = sqlx::query_scalar::<_, Uuid>(
            "SELECT family.id FROM refresh_token_family AS family
             JOIN user_account AS account ON account.user_id = family.user_id
             WHERE family.id = $1 AND family.user_id = $2
               AND family.revoked_at IS NULL
               AND family.expires_at > statement_timestamp()
               AND account.deleted_at IS NULL AND NOT account.is_banned",
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(active.is_some())
    }

    pub async fn active_account(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        terms_version: &str,
        privacy_version: &str,
    ) -> Result<Option<ActiveAccount>, DatabaseError> {
        let row = sqlx::query_as::<_, (Uuid, String, bool, bool)>(
            "SELECT account.user_id, account.role, account.is_banned,
               (
                 account.role <> 'user'
                 OR (
                   EXISTS (
                     SELECT 1 FROM user_consent
                     WHERE user_id = account.user_id
                       AND consent_type = 'terms_of_service_acceptance'
                       AND granted = true AND withdrawn_at IS NULL
                       AND document_version = $2
                   )
                   AND EXISTS (
                     SELECT 1 FROM user_consent
                     WHERE user_id = account.user_id
                       AND consent_type = 'privacy_notice_acknowledgement'
                       AND granted = true AND withdrawn_at IS NULL
                       AND document_version = $3
                   )
                 )
               ) AS onboarding_complete
             FROM user_account AS account
             WHERE account.user_id = $1 AND account.deleted_at IS NULL
               AND EXISTS (
                 SELECT 1 FROM refresh_token_family AS session
                 WHERE session.id = $4 AND session.user_id = account.user_id
                   AND session.revoked_at IS NULL
                   AND session.expires_at > statement_timestamp()
               )",
        )
        .bind(user_id)
        .bind(terms_version)
        .bind(privacy_version)
        .bind(session_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        row.map(|(user_id, role, is_banned, onboarding_complete)| {
            Ok(ActiveAccount {
                user_id,
                role: AccountRole::parse(&role).map_err(|_| DatabaseError::QueryFailed)?,
                is_banned,
                onboarding_complete,
            })
        })
        .transpose()
    }

    pub fn database(&self) -> &Database {
        &self.database
    }
}

impl MobileSessionStore for MobileSessionRepository {
    fn create(
        &self,
        user_id: Uuid,
        token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        Box::pin(MobileSessionRepository::create(self, user_id, token))
    }

    fn rotate(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome> {
        Box::pin(MobileSessionRepository::rotate(self, jti, hash, next))
    }

    fn logout(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        jti: Uuid,
        hash: String,
        device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool> {
        Box::pin(MobileSessionRepository::logout(
            self, user_id, session_id, jti, hash, device_id,
        ))
    }

    fn list(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>> {
        Box::pin(async move {
            MobileSessionRepository::list(self, user_id, limit, cursor.as_ref()).await
        })
    }

    fn revoke(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>> {
        Box::pin(MobileSessionRepository::revoke(
            self,
            user_id,
            current_session_id,
            target_id,
        ))
    }

    fn active_account(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        terms_version: Arc<str>,
        privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>> {
        Box::pin(async move {
            MobileSessionRepository::active_account(
                self,
                user_id,
                session_id,
                terms_version.as_ref(),
                privacy_version.as_ref(),
            )
            .await
        })
    }
}

struct StoredRefreshToken {
    id: Uuid,
    user_id: Uuid,
    token_hash: String,
    family_id: Uuid,
    revoked: bool,
    rotated_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy)]
enum RevocationReason {
    Replay,
    Logout,
    LogoutAll,
    UserRevoked,
}

impl RevocationReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Logout => "logout",
            Self::LogoutAll => "logout_all",
            Self::UserRevoked => "user_revoked",
        }
    }
}

async fn lock_token(
    connection: &mut PgConnection,
    jti: Uuid,
    supplied_hash: &str,
    owner_id: Option<Uuid>,
) -> Result<Option<StoredRefreshToken>, DatabaseError> {
    let candidate = sqlx::query_as::<_, (Uuid, Uuid, String, Uuid, bool, Option<DateTime<Utc>>)>(
        "SELECT id, user_id, token_hash, family_id, revoked, rotated_at
         FROM refresh_tokens WHERE jti = $1",
    )
    .bind(jti)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let Some((id, user_id, token_hash, family_id, revoked, rotated_at)) = candidate else {
        return Ok(None);
    };
    let candidate = StoredRefreshToken {
        id,
        user_id,
        token_hash,
        family_id,
        revoked,
        rotated_at,
    };
    if owner_id.is_some_and(|owner_id| owner_id != candidate.user_id)
        || !same_hash(&candidate.token_hash, supplied_hash)
    {
        return Ok(None);
    }
    if !lock_active_account(connection, candidate.user_id).await? {
        return Ok(None);
    }
    let row = sqlx::query_as::<_, (Uuid, Uuid, String, Uuid, bool, Option<DateTime<Utc>>)>(
        "SELECT id, user_id, token_hash, family_id, revoked, rotated_at
         FROM refresh_tokens
         WHERE id = $1 AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(candidate.id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(row.map(
        |(id, user_id, token_hash, family_id, revoked, rotated_at)| StoredRefreshToken {
            id,
            user_id,
            token_hash,
            family_id,
            revoked,
            rotated_at,
        },
    ))
}

async fn lock_active_account(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> Result<bool, DatabaseError> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM user_account
         WHERE user_id = $1 AND deleted_at IS NULL AND NOT is_banned
         FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?
    .is_some())
}

async fn active_family(
    connection: &mut PgConnection,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<bool, DatabaseError> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM refresh_token_family
         WHERE id = $1 AND user_id = $2
           AND revoked_at IS NULL AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?
    .is_some())
}

async fn insert_token(
    connection: &mut PgConnection,
    user_id: Uuid,
    session_id: Uuid,
    token: &NewRefreshToken,
    parent_id: Option<Uuid>,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO refresh_tokens
         (id, user_id, family_id, parent_token_id, token_hash, jti, expires_at, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(token.id)
    .bind(user_id)
    .bind(session_id)
    .bind(parent_id)
    .bind(&token.hash)
    .bind(token.jti)
    .bind(token.expires_at)
    .bind(token.created_at)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn revoke_families(
    connection: &mut PgConnection,
    user_id: Uuid,
    reason: RevocationReason,
    session_id: Option<Uuid>,
) -> Result<u64, DatabaseError> {
    let families = sqlx::query(
        "UPDATE refresh_token_family
         SET revoked_at = clock_timestamp(), revocation_reason = $2
         WHERE user_id = $1 AND revoked_at IS NULL
           AND ($3::uuid IS NULL OR id = $3)",
    )
    .bind(user_id)
    .bind(reason.as_str())
    .bind(session_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE refresh_tokens SET revoked = true
         WHERE user_id = $1 AND revoked = false
           AND ($2::uuid IS NULL OR family_id = $2)",
    )
    .bind(user_id)
    .bind(session_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "DELETE FROM device_token
         WHERE user_id = $1 AND ($2::uuid IS NULL OR session_id = $2)",
    )
    .bind(user_id)
    .bind(session_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(families.rows_affected())
}

fn same_hash(stored: &str, supplied: &str) -> bool {
    stored.as_bytes().ct_eq(supplied.as_bytes()).into()
}

fn random_uuid_v4() -> Uuid {
    Uuid::new_v4()
}
