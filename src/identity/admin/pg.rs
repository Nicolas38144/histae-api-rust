use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

use super::domain::{
    ActiveSessionRow, AdminRole, AuthEventRow, BootstrapRow, ChallengePurpose, ChallengeRow,
    CredentialRevocation, CredentialRow, EventCursor, NewCredential, NewSession, SessionSummaryRow,
};

pub type AdminStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct NewChallenge {
    pub purpose: ChallengePurpose,
    pub challenge_hash: [u8; 32],
    pub ceremony_state: Vec<u8>,
    pub user_id: Option<Uuid>,
    pub bootstrap_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
}

pub trait AdminAuthStore: Send + Sync {
    fn bootstrap(
        &self,
        id: Uuid,
        secret_hash: [u8; 32],
    ) -> AdminStoreFuture<'_, Option<BootstrapRow>>;
    fn create_challenge(&self, challenge: NewChallenge) -> AdminStoreFuture<'_, Uuid>;
    fn consume_challenge(
        &self,
        id: Uuid,
        purpose: ChallengePurpose,
        user_id: Option<Uuid>,
        bootstrap_id: Option<Uuid>,
    ) -> AdminStoreFuture<'_, Option<ChallengeRow>>;
    fn active_credentials(&self, user_id: Uuid) -> AdminStoreFuture<'_, Vec<CredentialRow>>;
    fn active_credential_by_external_id(
        &self,
        credential_id: String,
    ) -> AdminStoreFuture<'_, Option<CredentialRow>>;
    fn complete_bootstrap(
        &self,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        credential: NewCredential,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn add_credential(
        &self,
        user_id: Uuid,
        credential: NewCredential,
    ) -> AdminStoreFuture<'_, bool>;
    fn complete_authentication(
        &self,
        credential_id: Uuid,
        expected_counter: u32,
        next_counter: u32,
        device_type: String,
        backed_up: bool,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn active_session(
        &self,
        token_hash: [u8; 32],
        idle_ttl_millis: i64,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn revoke_session(&self, user_id: Uuid, session_id: Uuid) -> AdminStoreFuture<'_, ()>;
    fn revoke_other_sessions(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, u64>;
    fn active_sessions(&self, user_id: Uuid) -> AdminStoreFuture<'_, Vec<SessionSummaryRow>>;
    fn revoke_selected_session(
        &self,
        user_id: Uuid,
        target_session_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, bool>;
    fn rename_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
        name: String,
    ) -> AdminStoreFuture<'_, bool>;
    fn auth_events(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<EventCursor>,
    ) -> AdminStoreFuture<'_, Vec<AuthEventRow>>;
    fn revoke_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, CredentialRevocation>;
    fn issue_bootstrap(
        &self,
        user_id: Uuid,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        expires_at: DateTime<Utc>,
    ) -> AdminStoreFuture<'_, bool>;
}

#[derive(Clone)]
pub struct AdminAuthRepository {
    database: Database,
}

impl AdminAuthRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn bootstrap_impl(
        &self,
        id: Uuid,
        secret_hash: [u8; 32],
    ) -> Result<Option<BootstrapRow>, DatabaseError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT bootstrap.id, bootstrap.user_id
             FROM admin_webauthn_bootstrap AS bootstrap
             JOIN user_account AS account ON account.user_id = bootstrap.user_id
             WHERE bootstrap.id = $1 AND bootstrap.secret_hash = $2
               AND bootstrap.consumed_at IS NULL AND bootstrap.expires_at > clock_timestamp()
               AND account.role IN ('admin', 'superadmin')
               AND account.deleted_at IS NULL AND account.is_banned = false",
        )
        .bind(id)
        .bind(secret_hash.as_slice())
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(|(id, user_id)| BootstrapRow { id, user_id }))
    }

    async fn create_challenge_impl(&self, input: NewChallenge) -> Result<Uuid, DatabaseError> {
        sqlx::query_scalar(
            "INSERT INTO admin_webauthn_challenge
             (purpose, challenge_hash, ceremony_state, user_id, bootstrap_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(input.purpose.as_str())
        .bind(input.challenge_hash.as_slice())
        .bind(input.ceremony_state)
        .bind(input.user_id)
        .bind(input.bootstrap_id)
        .bind(input.expires_at)
        .fetch_one(self.database.pool())
        .await
        .map_err(map_sqlx_error)
    }

    async fn consume_challenge_impl(
        &self,
        id: Uuid,
        purpose: ChallengePurpose,
        user_id: Option<Uuid>,
        bootstrap_id: Option<Uuid>,
    ) -> Result<Option<ChallengeRow>, DatabaseError> {
        let state = sqlx::query_scalar::<_, Vec<u8>>(
            "UPDATE admin_webauthn_challenge SET consumed_at = clock_timestamp()
             WHERE id = $1 AND purpose = $2 AND consumed_at IS NULL
               AND expires_at > clock_timestamp() AND ceremony_state IS NOT NULL
               AND user_id IS NOT DISTINCT FROM $3::uuid
               AND bootstrap_id IS NOT DISTINCT FROM $4::uuid
             RETURNING ceremony_state",
        )
        .bind(id)
        .bind(purpose.as_str())
        .bind(user_id)
        .bind(bootstrap_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(state.map(|ceremony_state| ChallengeRow { ceremony_state }))
    }

    async fn active_credentials_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<CredentialRow>, DatabaseError> {
        let rows = sqlx::query_as::<_, CredentialTuple>(
            "SELECT credential.id, credential.user_id, account.role, credential.credential_id,
               credential.public_key, credential.counter, credential.device_type,
               credential.backed_up, credential.transports, credential.aaguid,
               credential.name, credential.created_at, credential.last_used_at
             FROM admin_webauthn_credential AS credential
             JOIN user_account AS account ON account.user_id = credential.user_id
             WHERE credential.user_id = $1 AND credential.revoked_at IS NULL
               AND account.role IN ('admin', 'superadmin')
               AND account.deleted_at IS NULL AND account.is_banned = false
             ORDER BY credential.created_at, credential.id",
        )
        .bind(user_id)
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        rows.into_iter().map(credential_row).collect()
    }

    async fn active_credential_by_external_id_impl(
        &self,
        credential_id: String,
    ) -> Result<Option<CredentialRow>, DatabaseError> {
        let row = sqlx::query_as::<_, CredentialTuple>(
            "SELECT credential.id, credential.user_id, account.role, credential.credential_id,
               credential.public_key, credential.counter, credential.device_type,
               credential.backed_up, credential.transports, credential.aaguid,
               credential.name, credential.created_at, credential.last_used_at
             FROM admin_webauthn_credential AS credential
             JOIN user_account AS account ON account.user_id = credential.user_id
             WHERE credential.credential_id = $1 AND credential.revoked_at IS NULL
               AND account.role IN ('admin', 'superadmin')
               AND account.deleted_at IS NULL AND account.is_banned = false",
        )
        .bind(credential_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        row.map(credential_row).transpose()
    }

    async fn complete_bootstrap_impl(
        &self,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        credential: NewCredential,
        session: NewSession,
    ) -> Result<Option<ActiveSessionRow>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let bootstrap = sqlx::query_as::<_, (Uuid, String)>(
                        "UPDATE admin_webauthn_bootstrap AS bootstrap
                         SET consumed_at = clock_timestamp()
                         FROM user_account AS account
                         WHERE bootstrap.id = $1 AND bootstrap.secret_hash = $2
                           AND bootstrap.user_id = account.user_id
                           AND bootstrap.consumed_at IS NULL
                           AND bootstrap.expires_at > clock_timestamp()
                           AND account.role IN ('admin', 'superadmin')
                           AND account.deleted_at IS NULL AND account.is_banned = false
                         RETURNING bootstrap.user_id, account.role",
                    )
                    .bind(bootstrap_id)
                    .bind(secret_hash.as_slice())
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some((user_id, role)) = bootstrap else {
                        return Ok(None);
                    };
                    let role = AdminRole::parse(&role).map_err(|_| DatabaseError::QueryFailed)?;
                    let credential_id = insert_credential(connection, user_id, credential).await?;
                    let created =
                        insert_session(connection, user_id, role, credential_id, session).await?;
                    insert_event(
                        connection,
                        user_id,
                        Some(credential_id),
                        Some(created.id),
                        "bootstrap_registered",
                    )
                    .await?;
                    Ok(Some(created))
                })
            })
            .await
    }

    async fn add_credential_impl(
        &self,
        user_id: Uuid,
        credential: NewCredential,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let active = sqlx::query_scalar::<_, String>(
                        "SELECT role FROM user_account
                         WHERE user_id = $1 AND role IN ('admin', 'superadmin')
                           AND deleted_at IS NULL AND is_banned = false
                         FOR UPDATE",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if active.is_none() {
                        return Ok(false);
                    }
                    let credential_id = insert_credential(connection, user_id, credential).await?;
                    insert_event(
                        connection,
                        user_id,
                        Some(credential_id),
                        None,
                        "credential_added",
                    )
                    .await?;
                    Ok(true)
                })
            })
            .await
    }

    async fn complete_authentication_impl(
        &self,
        credential_id: Uuid,
        expected_counter: u32,
        next_counter: u32,
        device_type: String,
        backed_up: bool,
        session: NewSession,
    ) -> Result<Option<ActiveSessionRow>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let locked = sqlx::query_as::<_, (i64, Uuid, String)>(
                        "SELECT credential.counter, credential.user_id, account.role
                         FROM admin_webauthn_credential AS credential
                         JOIN user_account AS account ON account.user_id = credential.user_id
                         WHERE credential.id = $1 AND credential.revoked_at IS NULL
                           AND account.role IN ('admin', 'superadmin')
                           AND account.deleted_at IS NULL AND account.is_banned = false
                         FOR UPDATE OF credential",
                    )
                    .bind(credential_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some((counter, user_id, role)) = locked else {
                        return Ok(None);
                    };
                    if counter != i64::from(expected_counter) {
                        return Ok(None);
                    }
                    let role = AdminRole::parse(&role).map_err(|_| DatabaseError::QueryFailed)?;
                    sqlx::query(
                        "UPDATE admin_webauthn_credential
                         SET counter = $2, device_type = $3, backed_up = $4,
                             last_used_at = clock_timestamp() WHERE id = $1",
                    )
                    .bind(credential_id)
                    .bind(i64::from(next_counter))
                    .bind(device_type)
                    .bind(backed_up)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let created =
                        insert_session(connection, user_id, role, credential_id, session).await?;
                    insert_event(
                        connection,
                        user_id,
                        Some(credential_id),
                        Some(created.id),
                        "login_succeeded",
                    )
                    .await?;
                    Ok(Some(created))
                })
            })
            .await
    }

    async fn active_session_impl(
        &self,
        token_hash: [u8; 32],
        idle_ttl_millis: i64,
    ) -> Result<Option<ActiveSessionRow>, DatabaseError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>)>(
            "UPDATE admin_session AS session
             SET last_seen_at = clock_timestamp(),
                 idle_expires_at = LEAST(session.absolute_expires_at,
                   clock_timestamp() + ($2::bigint * INTERVAL '1 millisecond'))
             FROM user_account AS account, admin_webauthn_credential AS credential
             WHERE session.token_hash = $1 AND session.user_id = account.user_id
               AND session.credential_id = credential.id
               AND credential.user_id = session.user_id AND credential.revoked_at IS NULL
               AND session.revoked_at IS NULL
               AND session.idle_expires_at > clock_timestamp()
               AND session.absolute_expires_at > clock_timestamp()
               AND account.role IN ('admin', 'superadmin')
               AND account.deleted_at IS NULL AND account.is_banned = false
             RETURNING session.id, session.user_id, session.credential_id, account.role,
               session.authenticated_at,
               LEAST(session.idle_expires_at, session.absolute_expires_at)",
        )
        .bind(token_hash.as_slice())
        .bind(idle_ttl_millis)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        row.map(active_session_row).transpose()
    }

    async fn revoke_session_impl(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), DatabaseError> {
        self.database.transaction(|connection| Box::pin(async move {
            let row = sqlx::query_scalar::<_, Uuid>(
                "UPDATE admin_session SET revoked_at = COALESCE(revoked_at, clock_timestamp())
                 WHERE id = $1 AND user_id = $2 RETURNING id",
            ).bind(session_id).bind(user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
            if row.is_some() {
                insert_event(connection, user_id, None, Some(session_id), "logout").await?;
            }
            Ok(())
        })).await
    }

    async fn revoke_other_sessions_impl(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<u64, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "UPDATE admin_session SET revoked_at = clock_timestamp()
                 WHERE user_id = $1 AND id <> $2 AND revoked_at IS NULL",
                    )
                    .bind(user_id)
                    .bind(current_session_id)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    insert_event(
                        connection,
                        user_id,
                        None,
                        Some(current_session_id),
                        "other_sessions_revoked",
                    )
                    .await?;
                    Ok(result.rows_affected())
                })
            })
            .await
    }

    async fn active_sessions_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<SessionSummaryRow>, DatabaseError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                Uuid,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "SELECT session.id, session.credential_id, credential.name,
               session.authenticated_at, session.last_seen_at,
               LEAST(session.idle_expires_at, session.absolute_expires_at)
             FROM admin_session AS session
             JOIN admin_webauthn_credential AS credential ON credential.id = session.credential_id
             WHERE session.user_id = $1 AND session.revoked_at IS NULL
               AND session.idle_expires_at > clock_timestamp()
               AND session.absolute_expires_at > clock_timestamp()
               AND credential.revoked_at IS NULL
             ORDER BY session.last_seen_at DESC, session.id DESC",
        )
        .bind(user_id)
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|row| SessionSummaryRow {
                id: row.0,
                credential_id: row.1,
                credential_name: row.2,
                authenticated_at: row.3,
                last_seen_at: row.4,
                expires_at: row.5,
            })
            .collect())
    }

    async fn revoke_selected_session_impl(
        &self,
        user_id: Uuid,
        target_session_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "UPDATE admin_session SET revoked_at = clock_timestamp()
                 WHERE id = $1 AND user_id = $2 AND id <> $3 AND revoked_at IS NULL
                   AND idle_expires_at > clock_timestamp()
                   AND absolute_expires_at > clock_timestamp()",
                    )
                    .bind(target_session_id)
                    .bind(user_id)
                    .bind(current_session_id)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if result.rows_affected() != 1 {
                        return Ok(false);
                    }
                    insert_event(
                        connection,
                        user_id,
                        None,
                        Some(target_session_id),
                        "session_revoked",
                    )
                    .await?;
                    Ok(true)
                })
            })
            .await
    }

    async fn rename_credential_impl(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
        name: String,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "UPDATE admin_webauthn_credential SET name = $3
                 WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
                    )
                    .bind(credential_id)
                    .bind(user_id)
                    .bind(name)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if result.rows_affected() != 1 {
                        return Ok(false);
                    }
                    insert_event(
                        connection,
                        user_id,
                        Some(credential_id),
                        Some(current_session_id),
                        "credential_renamed",
                    )
                    .await?;
                    Ok(true)
                })
            })
            .await
    }

    async fn auth_events_impl(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<EventCursor>,
    ) -> Result<Vec<AuthEventRow>, DatabaseError> {
        let (at, id) = cursor.map_or((None, None), |cursor| (Some(cursor.at), Some(cursor.id)));
        let rows = sqlx::query_as::<_, (Uuid, String, Option<Uuid>, Option<Uuid>, DateTime<Utc>)>(
            "SELECT id, event_type, credential_id, session_id, created_at
             FROM admin_auth_event WHERE user_id = $1
               AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4::uuid))
             ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(i64::from(limit))
        .bind(at)
        .bind(id)
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|row| AuthEventRow {
                id: row.0,
                event_type: row.1,
                credential_id: row.2,
                session_id: row.3,
                created_at: row.4,
            })
            .collect())
    }

    async fn revoke_credential_impl(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<CredentialRevocation, DatabaseError> {
        self.database.transaction(|connection| Box::pin(async move {
            let credentials = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM admin_webauthn_credential
                 WHERE user_id = $1 AND revoked_at IS NULL ORDER BY id FOR UPDATE",
            ).bind(user_id).fetch_all(&mut *connection).await.map_err(map_sqlx_error)?;
            if !credentials.contains(&credential_id) { return Ok(CredentialRevocation::NotFound); }
            if credentials.len() <= 1 { return Ok(CredentialRevocation::LastCredential); }
            sqlx::query("UPDATE admin_webauthn_credential SET revoked_at = clock_timestamp() WHERE id = $1")
                .bind(credential_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
            sqlx::query(
                "UPDATE admin_session SET revoked_at = clock_timestamp()
                 WHERE user_id = $1 AND id <> $2 AND revoked_at IS NULL",
            ).bind(user_id).bind(current_session_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
            insert_event(connection, user_id, Some(credential_id), Some(current_session_id), "credential_revoked").await?;
            Ok(CredentialRevocation::Revoked)
        })).await
    }

    async fn issue_bootstrap_impl(
        &self,
        user_id: Uuid,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        expires_at: DateTime<Utc>,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let account = sqlx::query_scalar::<_, Uuid>(
                        "SELECT user_id FROM user_account WHERE user_id = $1
                 AND role IN ('admin', 'superadmin') AND deleted_at IS NULL
                 AND is_banned = false FOR UPDATE",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if account.is_none() {
                        return Ok(false);
                    }
                    sqlx::query(
                        "UPDATE admin_webauthn_bootstrap SET consumed_at = clock_timestamp()
                 WHERE user_id = $1 AND consumed_at IS NULL",
                    )
                    .bind(user_id)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    sqlx::query(
                "INSERT INTO admin_webauthn_bootstrap (id, user_id, secret_hash, expires_at)
                 VALUES ($1, $2, $3, $4)",
            ).bind(bootstrap_id).bind(user_id).bind(secret_hash.as_slice()).bind(expires_at)
             .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                    insert_event(connection, user_id, None, None, "bootstrap_issued").await?;
                    Ok(true)
                })
            })
            .await
    }
}

type CredentialTuple = (
    Uuid,
    Uuid,
    String,
    String,
    Vec<u8>,
    i64,
    String,
    bool,
    Vec<String>,
    Option<Uuid>,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

fn credential_row(row: CredentialTuple) -> Result<CredentialRow, DatabaseError> {
    Ok(CredentialRow {
        id: row.0,
        user_id: row.1,
        role: AdminRole::parse(&row.2).map_err(|_| DatabaseError::QueryFailed)?,
        credential_id: row.3,
        public_key: row.4,
        counter: u32::try_from(row.5).map_err(|_| DatabaseError::QueryFailed)?,
        device_type: row.6,
        backed_up: row.7,
        transports: row.8,
        aaguid: row.9,
        name: row.10,
        created_at: row.11,
        last_used_at: row.12,
    })
}

fn active_session_row(
    row: (Uuid, Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>),
) -> Result<ActiveSessionRow, DatabaseError> {
    Ok(ActiveSessionRow {
        id: row.0,
        user_id: row.1,
        credential_id: row.2,
        role: AdminRole::parse(&row.3).map_err(|_| DatabaseError::QueryFailed)?,
        authenticated_at: row.4,
        expires_at: row.5,
    })
}

async fn insert_credential(
    connection: &mut PgConnection,
    user_id: Uuid,
    credential: NewCredential,
) -> Result<Uuid, DatabaseError> {
    sqlx::query_scalar(
        "INSERT INTO admin_webauthn_credential
         (user_id, credential_id, public_key, counter, device_type, backed_up,
          transports, aaguid, name)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(user_id)
    .bind(credential.credential_id)
    .bind(credential.public_key)
    .bind(i64::from(credential.counter))
    .bind(credential.device_type)
    .bind(credential.backed_up)
    .bind(credential.transports)
    .bind(credential.aaguid)
    .bind(credential.name)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)
}

async fn insert_session(
    connection: &mut PgConnection,
    user_id: Uuid,
    role: AdminRole,
    credential_id: Uuid,
    session: NewSession,
) -> Result<ActiveSessionRow, DatabaseError> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>)>(
        "INSERT INTO admin_session
         (user_id, credential_id, token_hash, idle_expires_at, absolute_expires_at)
         VALUES ($1,$2,$3,$4,$5)
         RETURNING id, user_id, credential_id, $6::text, authenticated_at,
           LEAST(idle_expires_at, absolute_expires_at)",
    )
    .bind(user_id)
    .bind(credential_id)
    .bind(session.token_hash.as_slice())
    .bind(session.idle_expires_at)
    .bind(session.absolute_expires_at)
    .bind(role.as_str())
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    active_session_row(row)
}

async fn insert_event(
    connection: &mut PgConnection,
    user_id: Uuid,
    credential_id: Option<Uuid>,
    session_id: Option<Uuid>,
    event_type: &'static str,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO admin_auth_event (user_id, credential_id, session_id, event_type)
         VALUES ($1,$2,$3,$4)",
    )
    .bind(user_id)
    .bind(credential_id)
    .bind(session_id)
    .bind(event_type)
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(map_sqlx_error)
}

impl AdminAuthStore for AdminAuthRepository {
    fn bootstrap(&self, id: Uuid, hash: [u8; 32]) -> AdminStoreFuture<'_, Option<BootstrapRow>> {
        Box::pin(self.bootstrap_impl(id, hash))
    }
    fn create_challenge(&self, value: NewChallenge) -> AdminStoreFuture<'_, Uuid> {
        Box::pin(self.create_challenge_impl(value))
    }
    fn consume_challenge(
        &self,
        id: Uuid,
        purpose: ChallengePurpose,
        user: Option<Uuid>,
        bootstrap: Option<Uuid>,
    ) -> AdminStoreFuture<'_, Option<ChallengeRow>> {
        Box::pin(self.consume_challenge_impl(id, purpose, user, bootstrap))
    }
    fn active_credentials(&self, user: Uuid) -> AdminStoreFuture<'_, Vec<CredentialRow>> {
        Box::pin(self.active_credentials_impl(user))
    }
    fn active_credential_by_external_id(
        &self,
        id: String,
    ) -> AdminStoreFuture<'_, Option<CredentialRow>> {
        Box::pin(self.active_credential_by_external_id_impl(id))
    }
    fn complete_bootstrap(
        &self,
        id: Uuid,
        hash: [u8; 32],
        credential: NewCredential,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>> {
        Box::pin(self.complete_bootstrap_impl(id, hash, credential, session))
    }
    fn add_credential(&self, user: Uuid, credential: NewCredential) -> AdminStoreFuture<'_, bool> {
        Box::pin(self.add_credential_impl(user, credential))
    }
    fn complete_authentication(
        &self,
        id: Uuid,
        expected: u32,
        next: u32,
        device: String,
        backed: bool,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>> {
        Box::pin(self.complete_authentication_impl(id, expected, next, device, backed, session))
    }
    fn active_session(
        &self,
        hash: [u8; 32],
        ttl: i64,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>> {
        Box::pin(self.active_session_impl(hash, ttl))
    }
    fn revoke_session(&self, user: Uuid, session: Uuid) -> AdminStoreFuture<'_, ()> {
        Box::pin(self.revoke_session_impl(user, session))
    }
    fn revoke_other_sessions(&self, user: Uuid, current: Uuid) -> AdminStoreFuture<'_, u64> {
        Box::pin(self.revoke_other_sessions_impl(user, current))
    }
    fn active_sessions(&self, user: Uuid) -> AdminStoreFuture<'_, Vec<SessionSummaryRow>> {
        Box::pin(self.active_sessions_impl(user))
    }
    fn revoke_selected_session(
        &self,
        user: Uuid,
        target: Uuid,
        current: Uuid,
    ) -> AdminStoreFuture<'_, bool> {
        Box::pin(self.revoke_selected_session_impl(user, target, current))
    }
    fn rename_credential(
        &self,
        user: Uuid,
        credential: Uuid,
        current: Uuid,
        name: String,
    ) -> AdminStoreFuture<'_, bool> {
        Box::pin(self.rename_credential_impl(user, credential, current, name))
    }
    fn auth_events(
        &self,
        user: Uuid,
        limit: u32,
        cursor: Option<EventCursor>,
    ) -> AdminStoreFuture<'_, Vec<AuthEventRow>> {
        Box::pin(self.auth_events_impl(user, limit, cursor))
    }
    fn revoke_credential(
        &self,
        user: Uuid,
        credential: Uuid,
        current: Uuid,
    ) -> AdminStoreFuture<'_, CredentialRevocation> {
        Box::pin(self.revoke_credential_impl(user, credential, current))
    }
    fn issue_bootstrap(
        &self,
        user: Uuid,
        id: Uuid,
        hash: [u8; 32],
        expires: DateTime<Utc>,
    ) -> AdminStoreFuture<'_, bool> {
        Box::pin(self.issue_bootstrap_impl(user, id, hash, expires))
    }
}
