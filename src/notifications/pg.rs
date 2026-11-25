use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::devices::{DeviceError, DeviceStore, DeviceStoreFuture, MAX_DEVICES_PER_ACCOUNT};
use super::domain::{DevicePlatform, DeviceRecord, DeviceRegistration};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

type DeviceTuple = (
    Uuid,
    Option<Uuid>,
    String,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<String>,
);

#[derive(Clone)]
pub struct PgNotificationRepository {
    database: Database,
}

impl PgNotificationRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn register_device(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        registration: DeviceRegistration,
    ) -> Result<Option<DeviceRecord>, DeviceError> {
        for _ in 0..2 {
            match self
                .register_device_once(user_id, session_id, &registration)
                .await
            {
                Err(DeviceError::Database(
                    DatabaseError::SerializationFailure | DatabaseError::Deadlock,
                )) => {}
                result => return result,
            }
        }
        self.register_device_once(user_id, session_id, &registration)
            .await
    }

    async fn register_device_once(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        registration: &DeviceRegistration,
    ) -> Result<Option<DeviceRecord>, DeviceError> {
        let registration = registration.clone();
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    // Serialize registrations and notification enqueue for this account.
                    let account = sqlx::query_scalar::<_, Uuid>(
                        "SELECT user_id FROM user_account
                         WHERE user_id = $1 AND deleted_at IS NULL AND NOT is_banned
                         FOR UPDATE",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if account.is_none() {
                        return Ok(None);
                    }
                    let session = sqlx::query_scalar::<_, Uuid>(
                        "SELECT id FROM refresh_token_family
                         WHERE id = $1 AND user_id = $2
                           AND revoked_at IS NULL AND expires_at > clock_timestamp()
                         FOR SHARE",
                    )
                    .bind(session_id)
                    .bind(user_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if session.is_none() {
                        return Ok(None);
                    }

                    sqlx::query(
                        "DELETE FROM device_token d USING refresh_token_family session
                         WHERE d.user_id = $1 AND session.id = d.session_id
                           AND (session.revoked_at IS NOT NULL OR session.expires_at <= clock_timestamp())",
                    )
                    .bind(user_id)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;

                    let other_devices = sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM (
                           SELECT 1 FROM device_token WHERE user_id = $1
                             AND session_id IS DISTINCT FROM $2 AND token <> $3 LIMIT $4
                         ) AS other_devices",
                    )
                    .bind(user_id)
                    .bind(session_id)
                    .bind(&registration.token)
                    .bind(MAX_DEVICES_PER_ACCOUNT)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if other_devices == MAX_DEVICES_PER_ACCOUNT {
                        return Err(DeviceError::AccountLimitReached);
                    }

                    sqlx::query("DELETE FROM device_token WHERE session_id = $1 AND user_id = $2 AND token <> $3")
                        .bind(session_id)
                        .bind(user_id)
                        .bind(&registration.token)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;

                    let row = sqlx::query_as::<_, DeviceTuple>(
                        "WITH registered AS (
                           INSERT INTO device_token
                             (id, user_id, session_id, token, platform, app_version, created_at, last_used_at, language)
                           VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp(), clock_timestamp(), $7)
                           ON CONFLICT (token) DO UPDATE SET
                             id = CASE WHEN device_token.session_id IS NOT DISTINCT FROM EXCLUDED.session_id
                               THEN device_token.id ELSE EXCLUDED.id END,
                             session_id = EXCLUDED.session_id,
                             platform = EXCLUDED.platform,
                             app_version = EXCLUDED.app_version,
                             language = EXCLUDED.language,
                             last_used_at = clock_timestamp()
                           WHERE device_token.user_id = EXCLUDED.user_id
                           RETURNING id, session_id, platform, app_version, created_at, last_used_at, language,
                             OLD.id AS previous_id
                         ), discarded_deliveries AS (
                           DELETE FROM notification_push_delivery delivery USING registered
                           WHERE registered.previous_id <> registered.id AND delivery.device_id = registered.previous_id
                         )
                         SELECT id, session_id, platform, app_version, created_at, last_used_at, language FROM registered",
                    )
                    .bind(Uuid::new_v4())
                    .bind(user_id)
                    .bind(session_id)
                    .bind(registration.token)
                    .bind(registration.platform.as_str())
                    .bind(registration.app_version)
                    .bind(registration.language.map(|language| language.as_str()))
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    // The unique token is already owned by another account. Roll
                    // back any changes to the caller's existing registration.
                    .ok_or(DeviceError::TokenConflict)?;
                    Ok(Some(map_device(row)?))
                })
            })
            .await
    }

    pub async fn devices_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DeviceRecord>, DatabaseError> {
        sqlx::query_as::<_, DeviceTuple>(
            "SELECT id, session_id, platform, app_version, created_at, last_used_at, language
             FROM device_token
             WHERE user_id = $1
             ORDER BY last_used_at DESC NULLS LAST, id LIMIT $2",
        )
        .bind(user_id)
        .bind(MAX_DEVICES_PER_ACCOUNT)
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?
        .into_iter()
        .map(map_device)
        .collect()
    }

    pub async fn remove_device(
        &self,
        user_id: Uuid,
        device_id: Uuid,
    ) -> Result<bool, DatabaseError> {
        let result = sqlx::query("DELETE FROM device_token WHERE id = $1 AND user_id = $2")
            .bind(device_id)
            .bind(user_id)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        Ok(result.rows_affected() == 1)
    }
}

impl DeviceStore for PgNotificationRepository {
    fn register(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        registration: DeviceRegistration,
    ) -> DeviceStoreFuture<'_, Option<DeviceRecord>, DeviceError> {
        Box::pin(self.register_device(user_id, session_id, registration))
    }

    fn list(&self, user_id: Uuid) -> DeviceStoreFuture<'_, Vec<DeviceRecord>> {
        Box::pin(self.devices_for_user(user_id))
    }

    fn remove(&self, user_id: Uuid, device_id: Uuid) -> DeviceStoreFuture<'_, bool> {
        Box::pin(self.remove_device(user_id, device_id))
    }
}

fn map_device(row: DeviceTuple) -> Result<DeviceRecord, DatabaseError> {
    let (id, session_id, platform, app_version, created_at, last_used_at, language) = row;
    Ok(DeviceRecord {
        id,
        session_id,
        platform: DevicePlatform::parse(&platform).ok_or(DatabaseError::QueryFailed)?,
        app_version,
        language: language
            .map(|value| {
                crate::localization::Language::parse(&value).ok_or(DatabaseError::QueryFailed)
            })
            .transpose()?,
        created_at,
        last_used_at,
    })
}
