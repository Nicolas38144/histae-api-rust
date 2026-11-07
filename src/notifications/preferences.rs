//! Account-wide push choices and private, participant-scoped conversation mutes.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NotificationPreferences {
    pub messages: bool,
    pub new_matches: bool,
    pub continuations: bool,
    pub photos: bool,
    pub reminders: bool,
    pub account_security: bool,
    pub discreet: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferenceChanges {
    pub messages: Option<bool>,
    pub new_matches: Option<bool>,
    pub continuations: Option<bool>,
    pub photos: Option<bool>,
    pub reminders: Option<bool>,
    pub account_security: Option<bool>,
    pub discreet: Option<bool>,
}

impl PreferenceChanges {
    pub fn is_empty(&self) -> bool {
        self.messages.is_none() && self.new_matches.is_none() && self.continuations.is_none()
            && self.photos.is_none() && self.reminders.is_none()
            && self.account_security.is_none() && self.discreet.is_none()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMute {
    pub muted: bool,
    pub muted_until: Option<DateTime<Utc>>,
}

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

// Checking both participants and blocks also prevents using mutes as an existence oracle.
const MATCH_ACCESS: &str = "SELECT m.id FROM match_init m
    JOIN user_account a ON a.user_id=m.user1_id
    JOIN user_account b ON b.user_id=m.user2_id
    WHERE m.id=$2 AND $1 IN (m.user1_id,m.user2_id)
      AND m.status IN ('active','awaiting_continuation','confirmed')
      AND m.expires_at>clock_timestamp()
      AND a.deleted_at IS NULL AND NOT a.is_banned
      AND b.deleted_at IS NULL AND NOT b.is_banned
      AND NOT EXISTS (SELECT 1 FROM user_block
        WHERE (blocker_id=m.user1_id AND blocked_id=m.user2_id)
           OR (blocker_id=m.user2_id AND blocked_id=m.user1_id))";

impl NotificationPreferenceRepository {
    pub fn new(database: Database) -> Self { Self { database } }

    pub async fn get(&self, user: Uuid) -> Result<Option<NotificationPreferences>, DatabaseError> {
        sqlx::query("SELECT COALESCE(p.messages,true) AS messages,
                COALESCE(p.new_matches,true) AS new_matches,
                COALESCE(p.continuations,true) AS continuations,
                COALESCE(p.photos,true) AS photos, COALESCE(p.reminders,true) AS reminders,
                COALESCE(p.account_security,true) AS account_security,
                COALESCE(p.discreet,false) AS discreet
            FROM user_account a LEFT JOIN user_notification_preference p ON p.user_id=a.user_id
            WHERE a.user_id=$1 AND a.deleted_at IS NULL AND NOT a.is_banned")
            .bind(user).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?
            .map(preferences).transpose()
    }

    pub async fn update(&self, user: Uuid, changes: PreferenceChanges)
        -> Result<Option<NotificationPreferences>, DatabaseError> {
        // Each PATCH touches only supplied fields, including during concurrent device updates.
        sqlx::query("INSERT INTO user_notification_preference
                (user_id,messages,new_matches,continuations,photos,reminders,account_security,discreet)
            SELECT user_id,COALESCE($2,true),COALESCE($3,true),COALESCE($4,true),
                COALESCE($5,true),COALESCE($6,true),COALESCE($7,true),COALESCE($8,false)
            FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned
            ON CONFLICT (user_id) DO UPDATE SET
                messages=COALESCE($2,user_notification_preference.messages),
                new_matches=COALESCE($3,user_notification_preference.new_matches),
                continuations=COALESCE($4,user_notification_preference.continuations),
                photos=COALESCE($5,user_notification_preference.photos),
                reminders=COALESCE($6,user_notification_preference.reminders),
                account_security=COALESCE($7,user_notification_preference.account_security),
                discreet=COALESCE($8,user_notification_preference.discreet)
            RETURNING messages,new_matches,continuations,photos,reminders,account_security,discreet")
            .bind(user).bind(changes.messages).bind(changes.new_matches).bind(changes.continuations)
            .bind(changes.photos).bind(changes.reminders).bind(changes.account_security)
            .bind(changes.discreet).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?
            .map(preferences).transpose()
    }

    pub async fn get_mute(&self, user: Uuid, match_id: Uuid)
        -> Result<Option<ConversationMute>, DatabaseError> {
        let row = sqlx::query(&format!("SELECT
            COALESCE(p.muted AND (p.muted_until IS NULL OR p.muted_until>clock_timestamp()),false) AS muted,
            CASE WHEN p.muted AND p.muted_until>clock_timestamp() THEN p.muted_until END AS muted_until
            FROM ({MATCH_ACCESS}) m LEFT JOIN match_notification_preference p
              ON p.match_id=m.id AND p.user_id=$1"))
            .bind(user).bind(match_id).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?;
        row.map(|r| Ok(ConversationMute {
            muted: r.try_get("muted").map_err(map_sqlx_error)?,
            muted_until: r.try_get("muted_until").map_err(map_sqlx_error)?,
        })).transpose()
    }

    pub async fn set_mute(&self, user: Uuid, match_id: Uuid, muted: bool,
        until: Option<DateTime<Utc>>) -> Result<Option<ConversationMute>, DatabaseError> {
        let row = sqlx::query(&format!("INSERT INTO match_notification_preference
            (user_id,match_id,muted,muted_until)
            SELECT $1,m.id,$3,$4 FROM ({MATCH_ACCESS}) m
            ON CONFLICT (match_id,user_id) DO UPDATE SET muted=EXCLUDED.muted,
                muted_until=EXCLUDED.muted_until RETURNING muted,muted_until"))
            .bind(user).bind(match_id).bind(muted).bind(until)
            .fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?;
        row.map(|r| Ok(ConversationMute {
            muted: r.try_get("muted").map_err(map_sqlx_error)?,
            muted_until: r.try_get("muted_until").map_err(map_sqlx_error)?,
        })).transpose()
    }
}
