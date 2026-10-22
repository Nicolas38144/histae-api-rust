use super::{StoreAccount, StoreError, StoreProvider, StoreSnapshot};
use crate::config::SecretString;
use crate::infra::{
    crypto::{open_secret, seal_secret, sha256_hex},
    postgres::{Database, map_sqlx_error},
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

const OBSERVED_AT_SQL: &str = r#"
SELECT clock_timestamp()
"#;

const UPSERT_ACCOUNT_SQL: &str = r#"
INSERT INTO billing_store_account(id, user_id, google_account_id) SELECT $1, user_id, $3
FROM user_account WHERE user_id=$2 AND deleted_at IS NULL AND NOT is_banned ON
CONFLICT(user_id) DO UPDATE SET user_id=EXCLUDED.user_id RETURNING id, google_account_id
"#;

const PURCHASE_BUSY_SQL: &str = r#"
SELECT EXISTS(SELECT 1 FROM effective_user_subscription WHERE user_id=$1 AND
access_granted) OR EXISTS(SELECT 1 FROM billing_store_subscription WHERE user_id=$1 AND
status IN ('pending', 'active', 'grace_period', 'past_due', 'on_hold', 'paused')) OR
EXISTS(SELECT 1 FROM user_subscription WHERE user_id=$1 AND provider='stripe' AND status
IN ('incomplete', 'trialing', 'active', 'past_due', 'unpaid', 'paused')) OR EXISTS(SELECT
1 FROM billing_checkout_session WHERE user_id=$1 AND expires_at>clock_timestamp() AND
(status IN ('creating', 'open') OR (status='failed' AND stripe_checkout_request IS NOT
NULL)))
"#;

const OWNER_SQL: &str = r#"
SELECT user_id FROM billing_store_account WHERE CASE WHEN $1='app_store' THEN id::text=$2
ELSE google_account_id=$2 END
"#;

const LOCK_OWNER_SQL: &str = r#"
SELECT user_id FROM user_account WHERE user_id=$1 AND deleted_at IS NULL FOR SHARE
"#;

const LOCK_ACCOUNT_SQL: &str = r#"
SELECT id FROM billing_store_account WHERE user_id=$1 FOR SHARE
"#;

const LINKED_OWNER_CONFLICT_SQL: &str = r#"
SELECT EXISTS(SELECT 1 FROM billing_store_subscription WHERE provider='google_play' AND
environment=$1 AND original_id=$2 AND account_id<>$3)
"#;

const APPLY_SUBSCRIPTION_SQL: &str = r#"
INSERT INTO billing_store_subscription(user_id, account_id, provider, original_id,
reference_encrypted, environment, product_id, base_plan_id, transaction_id,
billing_period, status, starts_at, expires_at, auto_renew, acknowledged, verified_at,
next_check_at) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
$16, $16+interval '6 hours') ON CONFLICT(provider, environment, original_id) DO UPDATE SET
reference_encrypted=EXCLUDED.reference_encrypted, product_id=EXCLUDED.product_id,
base_plan_id=EXCLUDED.base_plan_id, transaction_id=EXCLUDED.transaction_id,
billing_period=EXCLUDED.billing_period, status=EXCLUDED.status,
starts_at=EXCLUDED.starts_at, expires_at=EXCLUDED.expires_at,
auto_renew=EXCLUDED.auto_renew, acknowledged=billing_store_subscription.acknowledged OR
EXCLUDED.acknowledged, verified_at=EXCLUDED.verified_at,
next_check_at=EXCLUDED.next_check_at WHERE
billing_store_subscription.user_id=EXCLUDED.user_id AND
billing_store_subscription.account_id=EXCLUDED.account_id AND
billing_store_subscription.verified_at<=EXCLUDED.verified_at AND NOT
billing_store_subscription.superseded RETURNING id
"#;

const PREVIOUS_OWNER_SQL: &str = r#"
SELECT user_id FROM billing_store_subscription WHERE provider=$1 AND environment=$2 AND original_id=$3
"#;

const SUPERSEDE_LINKED_SQL: &str = r#"
UPDATE billing_store_subscription SET status='expired', auto_renew=false, superseded=true,
verified_at=$4 WHERE provider='google_play' AND original_id=$1 AND environment=$2 AND
original_id<>$3 AND verified_at<=$4 AND user_id=$5 AND account_id=$6
"#;

const RECEIVE_NOTIFICATION_SQL: &str = r#"
INSERT INTO billing_store_notification(provider, provider_event_id, reference_encrypted,
processed_at) VALUES($1, $2, $3, CASE WHEN $3 IS NULL THEN clock_timestamp() END) ON
CONFLICT(provider, provider_event_id) DO NOTHING RETURNING id
"#;

const NOTIFICATION_REFERENCE_SQL: &str = r#"
SELECT provider, reference_encrypted, NULL::uuid AS user_id FROM
billing_store_notification WHERE id=$1 AND processed_at IS NULL
"#;

const SUBSCRIPTION_REFERENCE_SQL: &str = r#"
SELECT provider, reference_encrypted, user_id FROM billing_store_subscription WHERE id=$1
AND user_id IS NOT NULL
"#;

const COMPLETE_NOTIFICATION_SQL: &str = r#"
UPDATE billing_store_notification SET processed_at=clock_timestamp(),reference_encrypted=NULL WHERE id=$1
"#;

const ACKNOWLEDGED_SQL: &str = r#"
UPDATE billing_store_subscription SET acknowledged=true WHERE id=$1
"#;

const DUE_SUBSCRIPTIONS_SQL: &str = r#"
SELECT id FROM billing_store_subscription WHERE user_id IS NOT NULL AND NOT superseded AND
next_check_at<=clock_timestamp() AND (expires_at>clock_timestamp()-interval '60 days' OR
NOT acknowledged) ORDER BY next_check_at, id LIMIT 100 FOR UPDATE SKIP LOCKED
"#;

const RESCHEDULE_SQL: &str = r#"
UPDATE billing_store_subscription SET next_check_at=clock_timestamp()+interval '6 hours' WHERE id=ANY($1)
"#;

const ENQUEUE_SQL: &str = r#"
INSERT INTO outbox_event(id, event_type, aggregate_id, payload) VALUES($3, $1, $2,
'{}'::jsonb) ON CONFLICT(event_type, aggregate_id) DO UPDATE SET status='pending',
attempts=0, available_at=clock_timestamp(), processed_at=NULL, last_error_code=NULL WHERE
outbox_event.status='completed'
"#;

const PURPOSE: &[u8] = b"histae-store-reference-v1";

#[derive(Clone)]
pub struct StoreRepository {
    pub(crate) database: Database,
    encryption: SecretString,
}
impl StoreRepository {
    pub fn new(database: Database, encryption: SecretString) -> Self {
        Self {
            database,
            encryption,
        }
    }
    pub async fn observed_at(&self) -> Result<DateTime<Utc>, StoreError> {
        sqlx::query_scalar(OBSERVED_AT_SQL)
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)
            .map_err(Into::into)
    }
    pub async fn account(&self, user_id: Uuid) -> Result<StoreAccount, StoreError> {
        let id = Uuid::new_v4();
        let google = sha256_hex(format!("histae-play:{id}").as_bytes());
        let row = sqlx::query(UPSERT_ACCOUNT_SQL)
            .bind(id)
            .bind(user_id)
            .bind(google)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?
            .ok_or(StoreError::AccountUnavailable)?;
        Ok(StoreAccount {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            google_id: row.try_get("google_account_id").map_err(map_sqlx_error)?,
        })
    }
    pub async fn purchase_allowed(&self, user_id: Uuid) -> Result<bool, StoreError> {
        let busy: bool = sqlx::query_scalar(PURCHASE_BUSY_SQL)
            .bind(user_id)
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        Ok(!busy)
    }
    pub async fn apply(
        &self,
        snapshot: StoreSnapshot,
        reference: String,
        expected_user: Option<Uuid>,
        observed_at: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StoreError> {
        let encrypted = seal_secret(&reference, &self.encryption, PURPOSE)
            .map_err(|_| StoreError::Unavailable)?;
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let owner: Option<Uuid> = sqlx::query_scalar(OWNER_SQL)
                        .bind(snapshot.provider.as_str())
                        .bind(&snapshot.account_binding)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .flatten();
                    let Some(owner) = owner else {
                        return Err(StoreError::Ownership);
                    };
                    if expected_user.is_some_and(|user| user != owner) {
                        return Err(StoreError::Ownership);
                    }
                    let alive: Option<Uuid> = sqlx::query_scalar(LOCK_OWNER_SQL)
                        .bind(owner)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if alive.is_none() {
                        return Err(StoreError::AccountUnavailable);
                    }
                    let account: Uuid = sqlx::query_scalar(LOCK_ACCOUNT_SQL)
                        .bind(owner)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if let Some(linked) = &snapshot.linked_reference_hash {
                        let conflict: bool = sqlx::query_scalar(LINKED_OWNER_CONFLICT_SQL)
                            .bind(&snapshot.environment)
                            .bind(linked)
                            .bind(account)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        if conflict {
                            return Err(StoreError::Ownership);
                        }
                    }
                    let row = sqlx::query(APPLY_SUBSCRIPTION_SQL)
                        .bind(owner)
                        .bind(account)
                        .bind(snapshot.provider.as_str())
                        .bind(&snapshot.original_id)
                        .bind(encrypted)
                        .bind(&snapshot.environment)
                        .bind(&snapshot.product_id)
                        .bind(&snapshot.base_plan_id)
                        .bind(&snapshot.transaction_id)
                        .bind(snapshot.billing_period.as_str())
                        .bind(&snapshot.status)
                        .bind(snapshot.starts_at)
                        .bind(snapshot.expires_at)
                        .bind(snapshot.auto_renew)
                        .bind(!snapshot.acknowledgement_required)
                        .bind(observed_at)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    let Some(row) = row else {
                        let previous_owner: Option<Uuid> = sqlx::query_scalar(PREVIOUS_OWNER_SQL)
                            .bind(snapshot.provider.as_str())
                            .bind(&snapshot.environment)
                            .bind(&snapshot.original_id)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?
                            .flatten();
                        return if previous_owner == Some(owner) {
                            Ok(None)
                        } else {
                            Err(StoreError::Ownership)
                        };
                    };
                    if let Some(linked) = snapshot.linked_reference_hash {
                        sqlx::query(SUPERSEDE_LINKED_SQL)
                            .bind(linked)
                            .bind(&snapshot.environment)
                            .bind(&snapshot.original_id)
                            .bind(observed_at)
                            .bind(owner)
                            .bind(account)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                    }
                    if snapshot.acknowledgement_required {
                        let id: Uuid = row.try_get("id").map_err(map_sqlx_error)?;
                        enqueue(connection, "billing.store.acknowledge", id).await?;
                    }
                    Ok(Some(owner))
                })
            })
            .await
    }
    pub async fn receive(
        &self,
        provider: StoreProvider,
        event_id: String,
        reference: Option<String>,
    ) -> Result<(), StoreError> {
        let encrypted = reference
            .as_deref()
            .map(|value| seal_secret(value, &self.encryption, PURPOSE))
            .transpose()
            .map_err(|_| StoreError::Unavailable)?;
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let id: Option<Uuid> = sqlx::query_scalar(RECEIVE_NOTIFICATION_SQL)
                        .bind(provider.as_str())
                        .bind(event_id)
                        .bind(encrypted)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if let Some(id) = id.filter(|_| reference.is_some()) {
                        enqueue(connection, "billing.store.notification", id).await?;
                    }
                    Ok(())
                })
            })
            .await
    }
    pub async fn reference(
        &self,
        id: Uuid,
        notification: bool,
    ) -> Result<Option<(StoreProvider, String, Option<Uuid>)>, StoreError> {
        let query = if notification {
            NOTIFICATION_REFERENCE_SQL
        } else {
            SUBSCRIPTION_REFERENCE_SQL
        };
        let row = sqlx::query(query)
            .bind(id)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        row.map(|row| {
            let provider: String = row.try_get("provider").map_err(map_sqlx_error)?;
            let encrypted: Vec<u8> = row.try_get("reference_encrypted").map_err(map_sqlx_error)?;
            let reference = open_secret(&encrypted, &self.encryption, PURPOSE)
                .map_err(|_| StoreError::Unavailable)?;
            Ok((
                StoreProvider::parse(&provider)?,
                reference,
                row.try_get("user_id").map_err(map_sqlx_error)?,
            ))
        })
        .transpose()
    }
    pub async fn complete_notification(&self, id: Uuid) -> Result<(), StoreError> {
        sqlx::query(COMPLETE_NOTIFICATION_SQL)
            .bind(id)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }
    pub async fn acknowledged(&self, id: Uuid) -> Result<(), StoreError> {
        sqlx::query(ACKNOWLEDGED_SQL)
            .bind(id)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }
    pub async fn schedule(&self) -> Result<u64, StoreError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let ids = sqlx::query_scalar::<_, Uuid>(DUE_SUBSCRIPTIONS_SQL)
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    for id in &ids {
                        enqueue(connection, "billing.store.reconcile", *id).await?;
                    }
                    sqlx::query(RESCHEDULE_SQL)
                        .bind(&ids)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    Ok(ids.len() as u64)
                })
            })
            .await
    }
}

async fn enqueue(
    connection: &mut sqlx::PgConnection,
    event_type: &str,
    id: Uuid,
) -> Result<(), StoreError> {
    sqlx::query(ENQUEUE_SQL)
        .bind(event_type)
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

impl super::StorePersistence for StoreRepository {
    fn observed_at(&self) -> super::StoreFuture<'_, DateTime<Utc>> {
        Box::pin(StoreRepository::observed_at(self))
    }
    fn account(&self, user: Uuid) -> super::StoreFuture<'_, StoreAccount> {
        Box::pin(StoreRepository::account(self, user))
    }
    fn purchase_allowed(&self, user: Uuid) -> super::StoreFuture<'_, bool> {
        Box::pin(StoreRepository::purchase_allowed(self, user))
    }
    fn apply(
        &self,
        snapshot: StoreSnapshot,
        reference: String,
        expected_user: Option<Uuid>,
        observed_at: DateTime<Utc>,
    ) -> super::StoreFuture<'_, Option<Uuid>> {
        Box::pin(StoreRepository::apply(
            self,
            snapshot,
            reference,
            expected_user,
            observed_at,
        ))
    }
    fn receive(
        &self,
        provider: StoreProvider,
        event_id: String,
        reference: Option<String>,
    ) -> super::StoreFuture<'_, ()> {
        Box::pin(StoreRepository::receive(
            self, provider, event_id, reference,
        ))
    }
    fn reference(
        &self,
        id: Uuid,
        notification: bool,
    ) -> super::StoreFuture<'_, Option<super::store::PurchaseReference>> {
        Box::pin(StoreRepository::reference(self, id, notification))
    }
    fn complete_notification(&self, id: Uuid) -> super::StoreFuture<'_, ()> {
        Box::pin(StoreRepository::complete_notification(self, id))
    }
    fn acknowledged(&self, id: Uuid) -> super::StoreFuture<'_, ()> {
        Box::pin(StoreRepository::acknowledged(self, id))
    }
}
