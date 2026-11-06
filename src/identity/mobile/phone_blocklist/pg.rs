//! Shared policy lock serializes final SMS checks, imports and manual decisions.
use super::domain::{
    Change, Decision, Entry, Overview, POLICY_LOCK, PhoneKey, PolicyError, SourceStatus, Target,
};
use crate::{
    identity::{admin_role::AdminRole, mobile::sms_safety::SmsSendRejection},
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub fn key_fingerprint(key: &crate::config::SecretString) -> Result<String, PolicyError> {
    crate::infra::crypto::hmac_sha256("histae:phone-blocklist:key-check:v1", key)
        .map_err(|_| PolicyError::Unavailable)
}
pub async fn initialize_key(
    database: &Database,
    key: &crate::config::SecretString,
) -> Result<(), PolicyError> {
    let fingerprint = key_fingerprint(key)?;
    database.transaction(|connection| { Box::pin(async move {
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(POLICY_LOCK).execute(&mut *connection).await.map_err(map_sqlx_error)?;
        sqlx::query("INSERT INTO phone_blocklist_control(singleton,key_fingerprint) VALUES(true,$1) ON CONFLICT(singleton) DO NOTHING")
            .bind(&fingerprint).execute(&mut *connection).await.map_err(map_sqlx_error)?;
        verify_key_on(connection,&fingerprint).await
    }) }).await
}
pub async fn check_key(
    database: &Database,
    key: &crate::config::SecretString,
) -> Result<(), PolicyError> {
    verify_key_on(database.acquire().await?.as_mut(), &key_fingerprint(key)?).await
}
pub(super) async fn verify_key_on(
    connection: &mut PgConnection,
    expected: &str,
) -> Result<(), PolicyError> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT key_fingerprint FROM phone_blocklist_control WHERE singleton")
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
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(POLICY_LOCK)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query_as::<_,(bool,bool,bool)>(
        "SELECT COALESCE(e.decision='allow' AND e.decision_until>clock_timestamp(),false),
         CASE WHEN e.decision_until>clock_timestamp() THEN e.decision='block' ELSE
           EXISTS(SELECT 1 FROM phone_blocklist_observation o WHERE o.entry_id=e.id AND o.last_seen_at>clock_timestamp()-INTERVAL '90 days') END,
         EXISTS(SELECT 1 FROM phone_blocklist_source WHERE last_success_at>clock_timestamp()-INTERVAL '7 days')
         FROM (VALUES(1)) placeholder(value) LEFT JOIN phone_blocklist_entry e ON e.phone_hash=$1"
    ).bind(hash).fetch_one(connection).await.map_err(map_sqlx_error)
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
        let sources = sqlx::query("SELECT * FROM phone_blocklist_source ORDER BY source")
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(|row| {
                Ok(SourceStatus {
                    source: row.try_get("source")?,
                    last_attempt_at: row.try_get("last_attempt_at")?,
                    last_success_at: row.try_get("last_success_at")?,
                    last_error: row.try_get("last_error")?,
                    imported_count: row.try_get("imported_count")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(map_sqlx_error)?;
        let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM phone_blocklist_source WHERE last_success_at>clock_timestamp()-INTERVAL '7 days')")
            .fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
        let total = sqlx::query_scalar("SELECT count(*) FROM phone_blocklist_entry")
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        let query =
            format!("{ENTRY_SELECT} ORDER BY e.created_at DESC,e.id DESC LIMIT $1 OFFSET $2");
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
        let mut connection = self.database.acquire().await?;
        let id: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM phone_blocklist_entry WHERE phone_hash=$1")
                .bind(key.hash)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
        match id {
            Some(id) => read_on(&mut connection, id).await,
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
        self.database.transaction(|connection| { Box::pin(async move {
            // Match retention's lock order: policy first, then account rows.
            sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(POLICY_LOCK)
                .execute(&mut *connection).await.map_err(map_sqlx_error)?;
            // Never trust the role captured in the HTTP session after a demotion.
            let role:Option<String>=sqlx::query_scalar("SELECT role FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE")
                .bind(change.actor).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
            let role=role.as_deref().and_then(AdminRole::parse).ok_or(PolicyError::Forbidden)?;
            if role==AdminRole::Admin && change.decision!=Decision::Allow {return Err(PolicyError::Forbidden);}
            let supplied=crate::shared::text::javascript_trim(change.reason.as_deref().unwrap_or_default());
            let reason=role.audit_reason(supplied,"Superadmin phone policy change");
            if !(3..=500).contains(&reason.encode_utf16().count()) || reason.chars().any(char::is_control)
                || change.version<0 || match change.decision {Decision::Automatic=>change.days.is_some(),_=>!change.days.is_some_and(|d|(1..=90).contains(&d))} {
                return Err(PolicyError::Invalid);
            }
            let id=match change.target {
                Target::Id(id)=>id,
                Target::Phone(key)=>{
                    sqlx::query("INSERT INTO phone_blocklist_entry(id,phone_hash,phone_hint) VALUES($1,$2,$3) ON CONFLICT(phone_hash) DO NOTHING")
                        .bind(Uuid::new_v4()).bind(&key.hash).bind(key.hint).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                    sqlx::query_scalar("SELECT id FROM phone_blocklist_entry WHERE phone_hash=$1")
                        .bind(key.hash).fetch_one(&mut *connection).await.map_err(map_sqlx_error)?
                }
            };
            let version:Option<i64>=sqlx::query_scalar("SELECT version FROM phone_blocklist_entry WHERE id=$1 FOR UPDATE")
                .bind(id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
            let version=version.ok_or(PolicyError::NotFound)?;
            if version!=change.version {return Err(PolicyError::Conflict);}
            sqlx::query("UPDATE phone_blocklist_entry SET decision=$2,decision_until=CASE WHEN $3::bigint IS NULL THEN NULL ELSE clock_timestamp()+$3*INTERVAL '1 day' END,version=version+1,updated_at=clock_timestamp() WHERE id=$1")
                .bind(id).bind(change.decision.as_str()).bind(change.days.map(i64::from)).execute(&mut *connection).await.map_err(map_sqlx_error)?;
            let action=match change.decision {Decision::Automatic=>"admin_reset_phone_policy",Decision::Allow=>"admin_allow_phone",Decision::Block=>"admin_block_phone"};
            sqlx::query("INSERT INTO data_access_log(accessed_user_id,accessor_id,accessor_role,action,reason) VALUES($1,$1,$2,$3,$4)")
                .bind(change.actor).bind(role.as_str()).bind(action).bind(format!("{reason} [policy:{id}]"))
                .execute(connection).await.map_err(map_sqlx_error)?;
            Ok(())
        }) }).await
    }
}

const ENTRY_SELECT: &str = r#"
SELECT e.id,e.phone_hint,e.decision,e.decision_until,e.version,
 CASE WHEN e.decision_until>clock_timestamp() THEN e.decision='block' ELSE
   EXISTS(SELECT 1 FROM phone_blocklist_observation o WHERE o.entry_id=e.id
          AND o.last_seen_at>clock_timestamp()-INTERVAL '90 days') END AS blocked,
 COALESCE((SELECT jsonb_agg(jsonb_build_object('source',o.source,'last_seen_at',o.last_seen_at,
          'expires_at',o.last_seen_at+INTERVAL '90 days') ORDER BY o.source)
          FROM phone_blocklist_observation o WHERE o.entry_id=e.id),'[]'::jsonb) AS observations
FROM phone_blocklist_entry e"#;

async fn read_on(connection: &mut PgConnection, id: Uuid) -> Result<Entry, PolicyError> {
    let row = sqlx::query(&format!("{ENTRY_SELECT} WHERE e.id=$1"))
        .bind(id)
        .fetch_optional(connection)
        .await
        .map_err(map_sqlx_error)?
        .ok_or(PolicyError::NotFound)?;
    map_entry(&row)
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
