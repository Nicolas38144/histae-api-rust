use super::{
    domain::{REWARD_CREDITS, RewardIntent, RewardPlatform, RewardStatus},
    error::RewardError,
    ssv::Callback,
    store::{RewardFuture, RewardStore},
};
use crate::{
    config::{AdvertisingConfig, LegalConfig},
    identity::pg_guards::lock_eligible_accounts,
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
    matches::{
        domain::ADVERTISING_STOCK_LIMIT,
        pg::credits::{lock_wallet, quota_on},
    },
};
use chrono::{DateTime, TimeDelta, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct RewardRepository {
    database: Database,
    config: AdvertisingConfig,
    legal: LegalConfig,
}

impl RewardStore for RewardRepository {
    fn enabled(&self) -> bool {
        RewardRepository::enabled(self)
    }

    fn create(&self, user_id: Uuid, platform: RewardPlatform) -> RewardFuture<'_, RewardIntent> {
        Box::pin(RewardRepository::create(self, user_id, platform))
    }

    fn status(&self, user_id: Uuid, id: Uuid) -> RewardFuture<'_, RewardIntent> {
        Box::pin(RewardRepository::status(self, user_id, id))
    }

    fn cancel(&self, user_id: Uuid, id: Uuid) -> RewardFuture<'_, ()> {
        Box::pin(RewardRepository::cancel(self, user_id, id))
    }

    fn complete(&self, callback: Callback) -> RewardFuture<'_, ()> {
        Box::pin(RewardRepository::complete(self, callback))
    }
}

impl RewardRepository {
    pub fn new(database: Database, config: AdvertisingConfig, legal: LegalConfig) -> Self {
        Self {
            database,
            config,
            legal,
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn cancel(&self, user_id: Uuid, id: Uuid) -> Result<(), RewardError> {
        self.database
            .transaction(|connection| Box::pin(cancel_on(connection, user_id, id)))
            .await
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        platform: RewardPlatform,
    ) -> Result<RewardIntent, RewardError> {
        if !self.config.enabled {
            return Err(RewardError::Disabled);
        }
        let ad_unit = match platform {
            RewardPlatform::Android => self.config.android_ad_unit.clone(),
            RewardPlatform::Ios => self.config.ios_ad_unit.clone(),
        };
        let legal = self.legal.clone();
        self.database
            .transaction(|connection| Box::pin(create_on(connection, user_id, ad_unit, legal)))
            .await
    }

    pub async fn status(&self, user_id: Uuid, id: Uuid) -> Result<RewardIntent, RewardError> {
        let row = sqlx::query(
            "SELECT id,ad_unit,expires_at,completed_at,cancelled_at,clock_timestamp() AS database_now
             FROM advertising_reward_intent
             WHERE id=$1 AND user_id=$2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(map_sqlx_error)?
        .ok_or(RewardError::NotFound)?;
        let complete = row
            .try_get::<Option<DateTime<Utc>>, _>("completed_at")
            .map_err(map_sqlx_error)?
            .is_some();
        let now = row.try_get("database_now").map_err(map_sqlx_error)?;
        let cancelled = row
            .try_get::<Option<DateTime<Utc>>, _>("cancelled_at")
            .map_err(map_sqlx_error)?
            .is_some();
        let mut intent = map_intent(row, complete, now)?;
        if cancelled {
            intent.status = RewardStatus::Cancelled;
        }
        Ok(intent)
    }

    pub async fn complete(&self, callback: Callback) -> Result<(), RewardError> {
        if !self.config.enabled {
            return Err(RewardError::Disabled);
        }
        if callback.reward_amount != REWARD_CREDITS {
            return Err(invalid_reward("admob_reward_amount_mismatch"));
        }
        if callback.reward_item != self.config.reward_item {
            return Err(invalid_reward("admob_reward_item_mismatch"));
        }
        self.database
            .transaction(|connection| Box::pin(complete_on(connection, callback)))
            .await
    }
}

fn invalid_reward(code: &'static str) -> RewardError {
    super::log_callback_rejection(code);
    RewardError::InvalidProof
}

fn map_intent(
    row: sqlx::postgres::PgRow,
    complete: bool,
    now: DateTime<Utc>,
) -> Result<RewardIntent, DatabaseError> {
    let id: Uuid = row.try_get("id").map_err(map_sqlx_error)?;
    let expires_at = row.try_get("expires_at").map_err(map_sqlx_error)?;
    Ok(RewardIntent {
        id,
        custom_data: id.to_string(),
        ad_unit: row.try_get("ad_unit").map_err(map_sqlx_error)?,
        expires_at,
        status: if complete {
            RewardStatus::Credited
        } else if expires_at <= now {
            RewardStatus::Expired
        } else {
            RewardStatus::Pending
        },
        reward_credits: REWARD_CREDITS,
    })
}

async fn cancel_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<(), RewardError> {
    lock_wallet(connection, user_id).await?;
    let result = sqlx::query(
        "UPDATE advertising_reward_intent SET cancelled_at=COALESCE(cancelled_at,clock_timestamp())
         WHERE id=$1 AND user_id=$2 AND completed_at IS NULL",
    )
    .bind(id)
    .bind(user_id)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    if result.rows_affected() != 1 {
        return Err(RewardError::NotFound);
    }
    Ok(())
}

async fn create_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    ad_unit: String,
    legal: LegalConfig,
) -> Result<RewardIntent, RewardError> {
    lock_eligible_accounts(connection, &[user_id], Some(&legal)).await?;
    lock_wallet(connection, user_id).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let quota = quota_on(connection, user_id, now).await?;
    if quota.weekly_limit.is_none() {
        return Err(RewardError::NotEligible);
    }
    if quota.advertising_balance >= ADVERTISING_STOCK_LIMIT {
        return Err(RewardError::StockFull);
    }
    // One outstanding intent reserves the last stock slot; retries return
    // that same intent, preventing concurrent ads from losing their reward.
    let outstanding = sqlx::query(
        "SELECT id, ad_unit, expires_at FROM advertising_reward_intent
         WHERE user_id = $1 AND completed_at IS NULL AND cancelled_at IS NULL
         AND expires_at + INTERVAL '24 hours' > $2
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(now)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if let Some(row) = outstanding {
        let expires: DateTime<Utc> = row.try_get("expires_at").map_err(map_sqlx_error)?;
        if expires > now {
            if row
                .try_get::<String, _>("ad_unit")
                .map_err(map_sqlx_error)?
                != ad_unit
            {
                return Err(RewardError::NotEligible);
            }
            return map_intent(row, false, now).map_err(Into::into);
        }
        // Preserve a slot for a signed late callback. Cancelling an abandoned
        // intent releases this slot immediately.
        return Err(RewardError::NotEligible);
    }
    let row = sqlx::query(
        "INSERT INTO advertising_reward_intent(user_id,ad_unit)
         VALUES($1,$2)
         RETURNING id,ad_unit,expires_at",
    )
    .bind(user_id)
    .bind(ad_unit)
    .fetch_one(connection)
    .await
    .map_err(map_sqlx_error)?;
    map_intent(row, false, now).map_err(Into::into)
}

async fn complete_on(
    connection: &mut sqlx::PgConnection,
    callback: Callback,
) -> Result<(), RewardError> {
    // Read ownership first; account and wallet locks always precede intent locks.
    let owner =
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM advertising_reward_intent WHERE id=$1")
            .bind(callback.intent_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .ok_or(RewardError::NotFound)?;
    // Honour a reward earned before a Premium upgrade or text version change;
    // erased/banned accounts cannot receive it.
    lock_eligible_accounts(connection, &[owner], None).await?;
    lock_wallet(connection, owner).await?;
    let row = sqlx::query(
        "SELECT ad_unit,created_at,expires_at,transaction_hash,cancelled_at,clock_timestamp() AS database_now
         FROM advertising_reward_intent
         WHERE id=$1
         FOR UPDATE",
    )
        .bind(callback.intent_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?
        .ok_or(RewardError::NotFound)?;
    if row
        .try_get::<Option<DateTime<Utc>>, _>("cancelled_at")
        .map_err(map_sqlx_error)?
        .is_some()
    {
        return Err(invalid_reward("admob_intent_cancelled"));
    }
    let expected: String = row.try_get("ad_unit").map_err(map_sqlx_error)?;
    if callback.ad_unit != expected
        && Some(callback.ad_unit.as_str()) != expected.rsplit('/').next()
    {
        return Err(invalid_reward("admob_ad_unit_mismatch"));
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(callback.transaction_id.to_ascii_lowercase().as_bytes())
    );
    if let Some(existing) = row
        .try_get::<Option<String>, _>("transaction_hash")
        .map_err(map_sqlx_error)?
    {
        return if existing == hash {
            Ok(())
        } else {
            Err(invalid_reward("admob_intent_already_credited"))
        };
    }
    let event = DateTime::from_timestamp_millis(callback.timestamp_ms)
        .ok_or_else(|| invalid_reward("admob_timestamp_out_of_range"))?;
    let created: DateTime<Utc> = row.try_get("created_at").map_err(map_sqlx_error)?;
    let expires: DateTime<Utc> = row.try_get("expires_at").map_err(map_sqlx_error)?;
    let now: DateTime<Utc> = row.try_get("database_now").map_err(map_sqlx_error)?;
    if event < created - TimeDelta::seconds(5) {
        return Err(invalid_reward("admob_event_before_intent"));
    }
    if event > expires {
        return Err(invalid_reward("admob_event_after_expiry"));
    }
    if event > now + TimeDelta::minutes(5) {
        return Err(invalid_reward("admob_event_in_future"));
    }
    if now > expires + TimeDelta::hours(24) {
        return Err(invalid_reward("admob_callback_too_late"));
    }
    // A globally unique transaction hash prevents cross-intent replay.
    let updated = sqlx::query(
        "UPDATE advertising_reward_intent SET completed_at=$2,transaction_hash=$3 WHERE id=$1",
    )
    .bind(callback.intent_id)
    .bind(now)
    .bind(hash)
    .execute(&mut *connection)
    .await;
    match updated {
        Ok(_) => {}
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|error| error.code().as_deref() == Some("23505")) =>
        {
            return Err(invalid_reward("admob_transaction_already_used"));
        }
        Err(error) => return Err(map_sqlx_error(error).into()),
    }
    let granted = sqlx::query(
        "INSERT INTO continuation_wallet(user_id,advertising_credits)
         VALUES($1,1)
         ON CONFLICT(user_id)
         DO UPDATE SET advertising_credits=continuation_wallet.advertising_credits+1
         WHERE continuation_wallet.advertising_credits<$2",
    )
    .bind(owner)
    .bind(ADVERTISING_STOCK_LIMIT)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    if granted.rows_affected() != 1 {
        return Err(RewardError::StockFull);
    }
    Ok(())
}
