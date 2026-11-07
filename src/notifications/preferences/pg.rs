use chrono::{DateTime, Utc};
use sqlx::Row as _;
use uuid::Uuid;

use super::{ConversationMute, NotificationPreferences, PreferenceChanges, queries};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

#[derive(Clone)]
pub struct NotificationPreferenceRepository {
    database: Database,
}

fn preferences(row: sqlx::postgres::PgRow) -> Result<NotificationPreferences, DatabaseError> {
    Ok(NotificationPreferences {
        messages: row.try_get("messages").map_err(map_sqlx_error)?,
        new_matches: row.try_get("new_matches").map_err(map_sqlx_error)?,
        continuations: row.try_get("continuations").map_err(map_sqlx_error)?,
        photos: row.try_get("photos").map_err(map_sqlx_error)?,
        reminders: row.try_get("reminders").map_err(map_sqlx_error)?,
        account_security: row.try_get("account_security").map_err(map_sqlx_error)?,
        discreet: row.try_get("discreet").map_err(map_sqlx_error)?,
    })
}

impl NotificationPreferenceRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn get(&self, user: Uuid) -> Result<Option<NotificationPreferences>, DatabaseError> {
        sqlx::query(queries::READ_PREFERENCES)
            .bind(user)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?
            .map(preferences)
            .transpose()
    }

    pub async fn update(
        &self,
        user: Uuid,
        changes: PreferenceChanges,
    ) -> Result<Option<NotificationPreferences>, DatabaseError> {
        // Each PATCH touches only supplied fields, including during concurrent device updates.
        sqlx::query(queries::UPDATE_PREFERENCES)
            .bind(user)
            .bind(changes.messages)
            .bind(changes.new_matches)
            .bind(changes.continuations)
            .bind(changes.photos)
            .bind(changes.reminders)
            .bind(changes.account_security)
            .bind(changes.discreet)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?
            .map(preferences)
            .transpose()
    }

    pub async fn get_mute(
        &self,
        user: Uuid,
        match_id: Uuid,
    ) -> Result<Option<ConversationMute>, DatabaseError> {
        let row = sqlx::query(&queries::READ_MUTE)
            .bind(user)
            .bind(match_id)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        row.map(conversation_mute).transpose()
    }

    pub async fn set_mute(
        &self,
        user: Uuid,
        match_id: Uuid,
        muted: bool,
        until: Option<DateTime<Utc>>,
    ) -> Result<Option<ConversationMute>, DatabaseError> {
        let row = sqlx::query(&queries::WRITE_MUTE)
            .bind(user)
            .bind(match_id)
            .bind(muted)
            .bind(until)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        row.map(conversation_mute).transpose()
    }
}

fn conversation_mute(row: sqlx::postgres::PgRow) -> Result<ConversationMute, DatabaseError> {
    Ok(ConversationMute {
        muted: row.try_get("muted").map_err(map_sqlx_error)?,
        muted_until: row.try_get("muted_until").map_err(map_sqlx_error)?,
    })
}
