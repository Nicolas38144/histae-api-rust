use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::{SmsControlError, SmsSafetyFuture, SmsSafetyStore, SmsSendRejection, SmsUsage};
use crate::{
    config::SmsSafetyConfig,
    infra::postgres::{Database, map_sqlx_error},
};

#[derive(Clone)]
pub struct PgSmsSafetyStore {
    database: Database,
}

impl PgSmsSafetyStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl SmsSafetyStore for PgSmsSafetyStore {
    fn reserve<'a>(
        &'a self,
        delivery_id: Uuid,
        phone_hash: String,
        policy: SmsSafetyConfig,
    ) -> SmsSafetyFuture<'a, Result<(), SmsSendRejection>> {
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                // Shared control lock: independent phones may reserve concurrently,
                // while a suspension waits for all previously authorized reservations.
                let suspended: bool = sqlx::query_scalar("SELECT suspended FROM sms_send_control WHERE singleton FOR SHARE")
                    .fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                if suspended { return Ok(Err(SmsSendRejection::Suspended)); }
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 5459283))")
                    .bind(&phone_hash).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                let counts: (i64, i64) = sqlx::query_as(
                    "SELECT count(*) FILTER (WHERE reserved_at > $2 - interval '1 hour'), count(*)
                     FROM sms_send_attempt WHERE phone_hash = $1 AND reserved_at > $2 - interval '24 hours'"
                ).bind(&phone_hash).bind(now).fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                if counts.1 >= i64::from(policy.daily_per_phone) { return Ok(Err(SmsSendRejection::DailyLimit)); }
                if counts.0 >= i64::from(policy.hourly_per_phone) { return Ok(Err(SmsSendRejection::HourlyLimit)); }
                let inserted: Option<Uuid> = sqlx::query_scalar(
                    "INSERT INTO sms_send_attempt(delivery_id, phone_hash, reserved_at) VALUES ($1, $2, $3)
                     ON CONFLICT (delivery_id) DO NOTHING RETURNING delivery_id"
                ).bind(delivery_id).bind(phone_hash).bind(now).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                if inserted.is_none() { return Ok(Err(SmsSendRejection::AlreadyReserved)); }
                // A bounded cleanup keeps accounting independent of OTP expiry,
                // logout and account deletion, without retaining it indefinitely.
                sqlx::query("DELETE FROM sms_send_attempt WHERE delivery_id IN (SELECT delivery_id FROM sms_send_attempt WHERE reserved_at <= $1 - interval '25 hours' ORDER BY reserved_at LIMIT 1000 FOR UPDATE SKIP LOCKED)")
                    .bind(now).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(Ok(()))
            })).await
        })
    }

    fn usage(&self) -> SmsSafetyFuture<'_, SmsUsage> {
        Box::pin(async move {
            let row = sqlx::query(
                "WITH instant AS MATERIALIZED (SELECT clock_timestamp() AS now), counts AS (
                   SELECT count(*) FILTER (WHERE reserved_at > now - interval '5 minutes') AS recent,
                     count(*) FILTER (WHERE reserved_at > now - interval '1 hour') AS hourly,
                     count(*) AS daily,
                     count(*) FILTER (WHERE reserved_at <= now - interval '5 minutes' AND reserved_at > now - interval '65 minutes') AS baseline
                   FROM sms_send_attempt CROSS JOIN instant WHERE reserved_at > now - interval '24 hours'
                 ) SELECT control.suspended, control.version, control.updated_at, counts.* FROM sms_send_control control CROSS JOIN counts WHERE singleton"
            ).fetch_one(self.database.pool()).await.map_err(map_sqlx_error)?;
            Ok(SmsUsage {
                manual_suspended: row.try_get("suspended").map_err(map_sqlx_error)?,
                version: row.try_get("version").map_err(map_sqlx_error)?,
                updated_at: row.try_get("updated_at").map_err(map_sqlx_error)?,
                last_5_minutes: row.try_get("recent").map_err(map_sqlx_error)?,
                last_hour: row.try_get("hourly").map_err(map_sqlx_error)?,
                last_day: row.try_get("daily").map_err(map_sqlx_error)?,
                previous_hour: row.try_get("baseline").map_err(map_sqlx_error)?,
            })
        })
    }

    fn set_suspended(
        &self,
        actor: Uuid,
        suspended: bool,
        version: i64,
    ) -> SmsSafetyFuture<'_, (), SmsControlError> {
        Box::pin(async move {
            self.database.transaction(|connection| Box::pin(async move {
                let role: Option<String> = sqlx::query_scalar("SELECT role FROM user_account WHERE user_id = $1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE")
                    .bind(actor).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                if role.as_deref() != Some("superadmin") { return Err(SmsControlError::Forbidden); }
                let changed = sqlx::query("UPDATE sms_send_control SET suspended = $1, version = version + 1, updated_at = clock_timestamp() WHERE singleton AND version = $2")
                    .bind(suspended).bind(version).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                if changed.rows_affected() != 1 { return Err(SmsControlError::Conflict); }
                sqlx::query("INSERT INTO data_access_log(accessed_user_id, accessor_id, accessor_role, action, reason) VALUES ($1, $1, 'superadmin', $2, $3)")
                    .bind(actor).bind(if suspended { "admin_suspend_sms" } else { "admin_resume_sms" })
                    .bind(if suspended { "SMS sending suspended" } else { "SMS sending resumed" })
                    .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(())
            })).await
        })
    }
}
