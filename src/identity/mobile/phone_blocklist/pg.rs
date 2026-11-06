//! Shared policy lock serializes final SMS checks, imports and manual decisions.
use super::{
    domain::{
        Change, Decision, Entry, Overview, POLICY_LOCK, PhoneKey, PolicyError, SourceStatus, Target,
    },
    queries,
};
use crate::{
    config::SecretString,
    identity::{admin_role::AdminRole, mobile::sms_safety::SmsSendRejection},
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub use super::pg_import::{import, record_failure};

pub(super) fn key_fingerprint(key: &SecretString) -> Result<String, PolicyError> {
    crate::infra::crypto::hmac_sha256("histae:phone-blocklist:key-check:v1", key)
        .map_err(|_| PolicyError::Unavailable)
}

pub async fn initialize_key(database: &Database, key: &SecretString) -> Result<(), PolicyError> {
    let fingerprint = key_fingerprint(key)?;
    database
        .transaction(|connection| {
            Box::pin(async move {
                sqlx::query(queries::EXCLUSIVE_LOCK)
                    .bind(POLICY_LOCK)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                sqlx::query(queries::INITIALIZE_KEY)
                    .bind(&fingerprint)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                verify_key_on(connection, &fingerprint).await
            })
        })
        .await
}

pub async fn check_key(database: &Database, key: &SecretString) -> Result<(), PolicyError> {
    verify_key_on(database.acquire().await?.as_mut(), &key_fingerprint(key)?).await
}

pub(super) async fn verify_key_on(
    connection: &mut PgConnection,
    expected: &str,
) -> Result<(), PolicyError> {
    let stored: Option<String> = sqlx::query_scalar(queries::READ_KEY)
        .fetch_optional(connection)
        .await
        .map_err(map_sqlx_error)?;
    if stored.as_deref() == Some(expected) {
        Ok(())
    } else {
        Err(PolicyError::KeyMismatch)
    }
}

pub async fn permit_on(
    connection: &mut PgConnection,
    hash: &str,
) -> Result<Result<(), SmsSendRejection>, DatabaseError> {
    let (allowed, blocked, ready) = check_on(connection, hash).await?;
    Ok(if allowed {
        Ok(())
    } else if blocked {
        Err(SmsSendRejection::PhoneBlocked)
    } else if !ready {
        Err(SmsSendRejection::Unavailable)
    } else {
        Ok(())
    })
}

/// Verification also rejects an OTP issued before a number became blocked.
/// An already accepted code does not depend on a new source fetch succeeding.
pub async fn otp_blocked_on(
    connection: &mut PgConnection,
    hash: &str,
) -> Result<bool, DatabaseError> {
    let (_, blocked, _) = check_on(connection, hash).await?;
    Ok(blocked)
}

async fn check_on(
    connection: &mut PgConnection,
    hash: &str,
) -> Result<(bool, bool, bool), DatabaseError> {
    sqlx::query(queries::SHARED_LOCK)
        .bind(POLICY_LOCK)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query_as(queries::CHECK_POLICY)
        .bind(hash)
        .fetch_one(connection)
        .await
        .map_err(map_sqlx_error)
}

#[derive(Clone)]
pub struct PgPhoneBlocklist {
    database: Database,
}

impl PgPhoneBlocklist {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn overview(&self, limit: u32, offset: u32) -> Result<Overview, PolicyError> {
        let mut connection = self.database.acquire().await?;
        let sources = sqlx::query(queries::SOURCES)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .iter()
            .map(map_source)
            .collect::<Result<Vec<_>, _>>()?;
        let ready = sqlx::query_scalar(queries::READY)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        let total = sqlx::query_scalar(queries::COUNT)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        let query = format!(
            "{} ORDER BY e.created_at DESC,e.id DESC LIMIT $1 OFFSET $2",
            queries::ENTRY
        );
        let entries = sqlx::query(&query)
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .iter()
            .map(map_entry)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Overview {
            entries,
            total,
            sources,
            ready,
        })
    }

    pub async fn lookup(&self, key: PhoneKey) -> Result<Entry, PolicyError> {
        let row = sqlx::query(&format!("{} WHERE e.phone_hash=$1", queries::ENTRY))
            .bind(key.hash)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        match row {
            Some(row) => map_entry(&row),
            None => Ok(Entry {
                id: None,
                phone_hint: key.hint,
                decision: Decision::Automatic,
                decision_until: None,
                version: 0,
                blocked: false,
                observations: vec![],
            }),
        }
    }

    pub async fn change(&self, change: Change) -> Result<(), PolicyError> {
        self.database
            .transaction(|connection| Box::pin(change_on(connection, change)))
            .await
    }
}

async fn change_on(connection: &mut PgConnection, change: Change) -> Result<(), PolicyError> {
    // Match retention's lock order: policy first, then account rows.
    sqlx::query(queries::EXCLUSIVE_LOCK)
        .bind(POLICY_LOCK)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    // Never trust the role captured in the HTTP session after a demotion.
    let role: Option<String> = sqlx::query_scalar(queries::ACTOR_ROLE)
        .bind(change.actor)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let role = role
        .as_deref()
        .and_then(AdminRole::parse)
        .ok_or(PolicyError::Forbidden)?;
    let reason = change.authorized_reason(role)?.to_owned();
    let id = resolve_target(connection, change.target).await?;
    let version: Option<i64> = sqlx::query_scalar(queries::LOCK_VERSION)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    if version.ok_or(PolicyError::NotFound)? != change.version {
        return Err(PolicyError::Conflict);
    }
    sqlx::query(queries::CHANGE)
        .bind(id)
        .bind(change.decision.as_str())
        .bind(change.days.map(i64::from))
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query(queries::AUDIT)
        .bind(change.actor)
        .bind(role.as_str())
        .bind(change.decision.audit_action())
        .bind(format!("{reason} [policy:{id}]"))
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn resolve_target(
    connection: &mut PgConnection,
    target: Target,
) -> Result<Uuid, PolicyError> {
    match target {
        Target::Id(id) => Ok(id),
        Target::Phone(key) => {
            sqlx::query(queries::INSERT_ENTRY)
                .bind(Uuid::new_v4())
                .bind(&key.hash)
                .bind(key.hint)
                .execute(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            sqlx::query_scalar(queries::ENTRY_ID)
                .bind(key.hash)
                .fetch_one(connection)
                .await
                .map_err(map_sqlx_error)
                .map_err(PolicyError::from)
        }
    }
}

fn map_source(row: &sqlx::postgres::PgRow) -> Result<SourceStatus, PolicyError> {
    Ok(SourceStatus {
        source: row.try_get("source").map_err(map_sqlx_error)?,
        last_attempt_at: row.try_get("last_attempt_at").map_err(map_sqlx_error)?,
        last_success_at: row.try_get("last_success_at").map_err(map_sqlx_error)?,
        last_error: row.try_get("last_error").map_err(map_sqlx_error)?,
        imported_count: row.try_get("imported_count").map_err(map_sqlx_error)?,
    })
}

fn map_entry(row: &sqlx::postgres::PgRow) -> Result<Entry, PolicyError> {
    let decision = match row
        .try_get::<String, _>("decision")
        .map_err(map_sqlx_error)?
        .as_str()
    {
        "allow" => Decision::Allow,
        "block" => Decision::Block,
        "automatic" => Decision::Automatic,
        _ => return Err(PolicyError::Unavailable),
    };
    Ok(Entry {
        id: Some(row.try_get("id").map_err(map_sqlx_error)?),
        phone_hint: row.try_get("phone_hint").map_err(map_sqlx_error)?,
        decision,
        decision_until: row.try_get("decision_until").map_err(map_sqlx_error)?,
        version: row.try_get("version").map_err(map_sqlx_error)?,
        blocked: row.try_get("blocked").map_err(map_sqlx_error)?,
        observations: serde_json::from_value(row.try_get("observations").map_err(map_sqlx_error)?)
            .map_err(|_| PolicyError::Unavailable)?,
    })
}
