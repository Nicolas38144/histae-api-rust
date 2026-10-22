use super::{WebBillingFuture, WebBillingStore, WebChallenge, WebIdentity};
use crate::{
    config::LegalConfig,
    infra::{
        crypto::sha256_hex,
        postgres::{Database, DatabaseError, map_sqlx_error},
    },
};
use sqlx::Row;
use uuid::Uuid;

const CREATE_CHALLENGE_SQL: &str = r#"
INSERT INTO web_billing_challenge(id, idempotency_key, requested_user_id, user_id,
phone_hash, language) VALUES($1, $2, $3, (SELECT user_id FROM user_account WHERE
user_id=$3 AND phone_number_hash=$4 AND deleted_at IS NULL AND NOT is_banned), $4, $5) ON
CONFLICT(idempotency_key) DO NOTHING RETURNING id, expires_at, user_id IS NOT NULL AS
matched
"#;

const EXISTING_CHALLENGE_SQL: &str = r#"
SELECT id, expires_at, user_id IS NOT NULL AS matched FROM web_billing_challenge WHERE
idempotency_key=$1 AND requested_user_id=$2 AND phone_hash=$3 AND language=$4
"#;

const BEGIN_VERIFY_SQL: &str = r#"
UPDATE web_billing_challenge SET attempts=attempts+1 WHERE id=$1 AND phone_hash=$2 AND NOT
consumed AND attempts<5 AND expires_at>clock_timestamp() RETURNING user_id
"#;

const CONSUME_CHALLENGE_SQL: &str = r#"
UPDATE web_billing_challenge SET consumed=true WHERE id=$1 AND user_id=$2 AND NOT consumed
AND expires_at>clock_timestamp() AND EXISTS(SELECT 1 FROM user_account WHERE user_id=$2
AND deleted_at IS NULL AND NOT is_banned)
"#;

const OPEN_SESSION_SQL: &str = r#"
INSERT INTO web_billing_session(token_hash,user_id) VALUES($1,$2)
"#;

const IDENTITY_SQL: &str = r#"
SELECT account.user_id, (account.role<>'user' OR (EXISTS(SELECT 1 FROM user_consent WHERE
user_id=account.user_id AND consent_type='terms_of_service_acceptance' AND granted AND
withdrawn_at IS NULL AND document_version=$2) AND EXISTS(SELECT 1 FROM user_consent WHERE
user_id=account.user_id AND consent_type='privacy_notice_acknowledgement' AND granted AND
withdrawn_at IS NULL AND document_version=$3))) AS onboarding_complete FROM
web_billing_session session JOIN user_account account ON account.user_id=session.user_id
WHERE session.token_hash=$1 AND session.expires_at>clock_timestamp() AND
account.deleted_at IS NULL AND NOT account.is_banned
"#;

const LOGOUT_SQL: &str = r#"
DELETE FROM web_billing_session WHERE token_hash=$1
"#;

#[derive(Clone)]
pub struct WebBillingRepository {
    database: Database,
    legal: LegalConfig,
}
impl WebBillingRepository {
    pub fn new(database: Database, legal: LegalConfig) -> Self {
        Self { database, legal }
    }
    pub async fn challenge(
        &self,
        key: Uuid,
        user_id: Uuid,
        phone_hash: &str,
        language: &str,
    ) -> Result<Option<WebChallenge>, DatabaseError> {
        let id = Uuid::new_v4();
        let row = sqlx::query(CREATE_CHALLENGE_SQL)
            .bind(id)
            .bind(key)
            .bind(user_id)
            .bind(phone_hash)
            .bind(language)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        let (row, created) = if let Some(row) = row {
            (Some(row), true)
        } else {
            (
                sqlx::query(EXISTING_CHALLENGE_SQL)
                    .bind(key)
                    .bind(user_id)
                    .bind(phone_hash)
                    .bind(language)
                    .fetch_optional(self.database.pool())
                    .await
                    .map_err(map_sqlx_error)?,
                false,
            )
        };
        row.map(|row| {
            Ok(WebChallenge {
                id: row.try_get("id").map_err(map_sqlx_error)?,
                expires_at: row.try_get("expires_at").map_err(map_sqlx_error)?,
                matched: row.try_get("matched").map_err(map_sqlx_error)?,
                created,
            })
        })
        .transpose()
    }
    pub async fn begin_verify(
        &self,
        id: Uuid,
        phone_hash: &str,
    ) -> Result<Option<Uuid>, DatabaseError> {
        // Consume an attempt before examining the code. Parallel invalid codes
        // cannot bypass the per-challenge budget.
        sqlx::query_scalar::<_, Option<Uuid>>(BEGIN_VERIFY_SQL)
            .bind(id)
            .bind(phone_hash)
            .fetch_optional(self.database.pool())
            .await
            .map(|value| value.flatten())
            .map_err(map_sqlx_error)
    }
    pub async fn open_session(
        &self,
        challenge: Uuid,
        user_id: Uuid,
        token: &str,
    ) -> Result<bool, DatabaseError> {
        let hash = sha256_hex(token.as_bytes());
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let changed = sqlx::query(CONSUME_CHALLENGE_SQL)
                        .bind(challenge)
                        .bind(user_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if changed.rows_affected() != 1 {
                        return Ok(false);
                    }
                    sqlx::query(OPEN_SESSION_SQL)
                        .bind(hash)
                        .bind(user_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    Ok(true)
                })
            })
            .await
    }
    pub async fn identity(&self, token: &str) -> Result<Option<WebIdentity>, DatabaseError> {
        let row = sqlx::query(IDENTITY_SQL)
            .bind(sha256_hex(token.as_bytes()))
            .bind(&self.legal.terms_version)
            .bind(&self.legal.privacy_version)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        row.map(|row| {
            Ok(WebIdentity {
                user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                onboarding_complete: row.try_get("onboarding_complete").map_err(map_sqlx_error)?,
            })
        })
        .transpose()
    }
    pub async fn logout(&self, token: &str) -> Result<(), DatabaseError> {
        sqlx::query(LOGOUT_SQL)
            .bind(sha256_hex(token.as_bytes()))
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }
}

impl WebBillingStore for WebBillingRepository {
    fn challenge<'a>(
        &'a self,
        key: Uuid,
        user: Uuid,
        phone_hash: &'a str,
        language: &'a str,
    ) -> WebBillingFuture<'a, Option<WebChallenge>> {
        Box::pin(WebBillingRepository::challenge(
            self, key, user, phone_hash, language,
        ))
    }
    fn begin_verify<'a>(
        &'a self,
        id: Uuid,
        phone_hash: &'a str,
    ) -> WebBillingFuture<'a, Option<Uuid>> {
        Box::pin(WebBillingRepository::begin_verify(self, id, phone_hash))
    }
    fn open_session<'a>(
        &'a self,
        challenge: Uuid,
        user: Uuid,
        token: &'a str,
    ) -> WebBillingFuture<'a, bool> {
        Box::pin(WebBillingRepository::open_session(
            self, challenge, user, token,
        ))
    }
    fn identity<'a>(&'a self, token: &'a str) -> WebBillingFuture<'a, Option<WebIdentity>> {
        Box::pin(WebBillingRepository::identity(self, token))
    }
    fn logout<'a>(&'a self, token: &'a str) -> WebBillingFuture<'a, ()> {
        Box::pin(WebBillingRepository::logout(self, token))
    }
}
