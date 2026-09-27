use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::devices::{DeviceStore, DeviceStoreFuture};
use super::domain::{DevicePlatform, DeviceRecord, DeviceRegistration};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

type DeviceTuple = (
    Uuid,
    Option<Uuid>,
    String,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
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
    ) -> Result<Option<DeviceRecord>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
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

                    let row = sqlx::query_as::<_, DeviceTuple>(
                        "INSERT INTO device_token
                           (id, user_id, session_id, token, platform, app_version, created_at, last_used_at)
                         SELECT $1, $2, id, $4, $5, $6, clock_timestamp(), clock_timestamp()
                         FROM refresh_token_family
                         WHERE id = $3 AND user_id = $2
                           AND revoked_at IS NULL AND expires_at > clock_timestamp()
                         ON CONFLICT (token) DO UPDATE SET
                           user_id = EXCLUDED.user_id,
                           session_id = EXCLUDED.session_id,
                           platform = EXCLUDED.platform,
                           app_version = EXCLUDED.app_version,
                           last_used_at = clock_timestamp()
                         RETURNING id, session_id, platform, app_version, created_at, last_used_at",
                    )
                    .bind(Uuid::new_v4())
                    .bind(user_id)
                    .bind(session_id)
                    .bind(registration.token)
                    .bind(registration.platform.as_str())
                    .bind(registration.app_version)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    row.map(map_device).transpose()
                })
            })
            .await
    }

    pub async fn devices_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DeviceRecord>, DatabaseError> {
        sqlx::query_as::<_, DeviceTuple>(
            "SELECT id, session_id, platform, app_version, created_at, last_used_at
             FROM device_token
             WHERE user_id = $1
             ORDER BY last_used_at DESC NULLS LAST, id",
        )
        .bind(user_id)
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

    pub fn database(&self) -> &Database {
        &self.database
    }
}

impl DeviceStore for PgNotificationRepository {
    fn register(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        registration: DeviceRegistration,
    ) -> DeviceStoreFuture<'_, Option<DeviceRecord>> {
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
    let (id, session_id, platform, app_version, created_at, last_used_at) = row;
    Ok(DeviceRecord {
        id,
        session_id,
        platform: DevicePlatform::parse(&platform).ok_or(DatabaseError::QueryFailed)?,
        app_version,
        created_at,
        last_used_at,
    })
}
