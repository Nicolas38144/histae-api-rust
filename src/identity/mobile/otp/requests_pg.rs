use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
    localization::Language,
    outbox::{pg::PgOutboxRepository, types::{NewOutboxEvent, OutboxEventType}},
};
use super::requests::{NewOtpRequest, OtpRequestStore, PendingOtpRequest, RequestError, RequestFuture};

#[derive(Clone)]
pub struct PgOtpRequestStore {
    database: Database,
    outbox: Arc<PgOutboxRepository>,
}

impl PgOtpRequestStore {
    pub fn new(database: Database) -> Self {
        Self { outbox: Arc::new(PgOutboxRepository::new(database.clone())), database }
    }
}

impl OtpRequestStore for PgOtpRequestStore {
    fn enqueue(&self, request: NewOtpRequest) -> RequestFuture<'_, u64, RequestError> {
        Box::pin(async move {
            let outbox = Arc::clone(&self.outbox);
            self.database.transaction(|connection| Box::pin(async move {
                // Key first, then phone: identical intents serialize across
                // instances and conflicting reuse cannot race a new enqueue.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 6837821))")
                    .bind(request.id.to_string()).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                let existing: Option<(String, bool, String, DateTime<Utc>)> = sqlx::query_as(
                    "SELECT phone_hash, registration, language, resend_at FROM mobile_otp_request WHERE id=$1"
                ).bind(request.id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
                let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                if let Some((hash, registration, language, resend_at)) = existing {
                    if hash != request.phone_hash || registration != request.registration || language != request.language.as_str() {
                        return Err(RequestError::Conflict);
                    }
                    return Ok(seconds_until(resend_at, now));
                }
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 6837822))")
                    .bind(&request.phone_hash).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                let resend_at: Option<DateTime<Utc>> = sqlx::query_scalar(
                    "SELECT max(resend_at) FROM mobile_otp_request WHERE phone_hash=$1 AND resend_at > $2"
                ).bind(&request.phone_hash).bind(now).fetch_one(&mut *connection).await.map_err(map_sqlx_error)?;
                if let Some(deadline) = resend_at { return Err(RequestError::Cooldown(seconds_until(deadline, now))); }
                let ttl = request.ttl.as_secs_f64();
                let cooldown = request.cooldown.as_secs_f64();
                sqlx::query(
                    "INSERT INTO mobile_otp_request(id,phone_hash,encrypted_phone,registration,language,created_at,expires_at,resend_at)
                     VALUES($1,$2,$3,$4,$5,$6,$6 + make_interval(secs => $7::double precision),$6 + make_interval(secs => $8::double precision))"
                ).bind(request.id).bind(request.phone_hash).bind(request.encrypted_phone).bind(request.registration)
                    .bind(request.language.as_str()).bind(now).bind(ttl).bind(cooldown)
                    .execute(&mut *connection).await.map_err(map_sqlx_error)?;
                outbox.enqueue(connection, &NewOutboxEvent::empty(OutboxEventType::MobileOtpSend, request.id)).await?;
                // Retain request metadata only for the code's lifetime. Payloads
                // are also erased when dispatched and by privacy maintenance.
                sqlx::query("DELETE FROM mobile_otp_request WHERE id IN (SELECT id FROM mobile_otp_request WHERE expires_at <= $1 AND resend_at <= $1 ORDER BY expires_at,id LIMIT 1000 FOR UPDATE SKIP LOCKED)")
                    .bind(now).execute(&mut *connection).await.map_err(map_sqlx_error)?;
                Ok(request.cooldown.as_secs())
            })).await
        })
    }

    fn pending(&self, id: Uuid) -> RequestFuture<'_, Option<PendingOtpRequest>> {
        Box::pin(async move {
            let row: Option<(String, Vec<u8>, bool, String)> = sqlx::query_as(
                "SELECT phone_hash, encrypted_phone, registration, language FROM mobile_otp_request r
                 WHERE id=$1 AND encrypted_phone IS NOT NULL AND expires_at > clock_timestamp()
                 AND NOT EXISTS (SELECT 1 FROM mobile_otp_request newer WHERE newer.phone_hash=r.phone_hash AND (newer.created_at,newer.id) > (r.created_at,r.id))"
            ).bind(id).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?;
            row.map(|(phone_hash, encrypted_phone, registration, language)| {
                let language = Language::parse(&language).ok_or(DatabaseError::QueryFailed)?;
                Ok(PendingOtpRequest { id, phone_hash, encrypted_phone, registration, language })
            }).transpose()
        })
    }

    fn finish(&self, id: Uuid) -> RequestFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query("UPDATE mobile_otp_request SET encrypted_phone=NULL WHERE id=$1")
                .bind(id).execute(self.database.pool()).await.map_err(map_sqlx_error)?;
            Ok(())
        })
    }
}

fn seconds_until(deadline: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    let milliseconds = (deadline - now).num_milliseconds().max(0) as u64;
    milliseconds.div_ceil(Duration::from_secs(1).as_millis() as u64)
}
