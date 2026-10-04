use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row as _;
use uuid::Uuid;

use super::domain::{
    AdminConsent, AdminPreferences, AdminPresence, AdminTrait, AdminUserDetailRow, AdminUserRole,
    AdminUserRow, AdminUserStatus, BanResult, CursorMatchRow, CursorMessageRow, PageCursor,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::matches::domain::{MatchRecord, MatchStatus, MessageRecord};
use crate::profiles::domain::{ConsentType, LookingFor, Sex};

use super::store::{AdministrationStore, AdministrationStoreFuture};

#[derive(Clone)]
pub struct PgAdministrationStore {
    database: Database,
}

impl PgAdministrationStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[allow(clippy::too_many_arguments)]
impl AdministrationStore for PgAdministrationStore {
    fn list_users(
        &self,
        status: Option<AdminUserStatus>,
        role: Option<AdminUserRole>,
        search: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
        terms_version: String,
        privacy_version: String,
    ) -> AdministrationStoreFuture<'_, Vec<AdminUserRow>> {
        Box::pin(async move {
            let searched_id = canonical_uuid(&search);
            let rows = sqlx::query(LIST_USERS_SQL)
                .bind(status.map(AdminUserStatus::as_str))
                .bind(role.map(AdminUserRole::as_str))
                .bind(&search)
                .bind(i64::from(limit))
                .bind(i64::from(offset))
                .bind(cursor.as_ref().map(|value| value.at))
                .bind(cursor.as_ref().map(|value| value.id))
                .bind(terms_version)
                .bind(privacy_version)
                .bind(searched_id)
                .fetch_all(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
            rows.into_iter().map(map_admin_user).collect()
        })
    }

    fn user_detail(
        &self,
        target_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        terms_version: String,
        privacy_version: String,
    ) -> AdministrationStoreFuture<'_, Option<AdminUserDetailRow>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let account = sqlx::query(DETAIL_USER_SQL)
                            .bind(target_id)
                            .bind(terms_version)
                            .bind(privacy_version)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        let Some(account) = account else {
                            return Ok(None);
                        };
                        let banned_reason =
                            account.try_get("banned_reason").map_err(map_sqlx_error)?;
                        let user = map_admin_user(account)?;
                        let preferences = sqlx::query(
                            "SELECT min_age, max_age, max_distance_km, looking_for
                             FROM user_preferences WHERE user_id = $1",
                        )
                        .bind(target_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .map(map_preferences)
                        .transpose()?;
                        let traits = sqlx::query(
                            "SELECT trait.id, trait.name FROM trait
                             JOIN user_trait ON user_trait.trait_id = trait.id
                             WHERE user_trait.user_id = $1 ORDER BY trait.name, trait.id",
                        )
                        .bind(target_id)
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .into_iter()
                        .map(|row| {
                            Ok(AdminTrait {
                                id: row.try_get("id").map_err(map_sqlx_error)?,
                                name: row.try_get("name").map_err(map_sqlx_error)?,
                            })
                        })
                        .collect::<Result<Vec<_>, DatabaseError>>()?;
                        let consents = sqlx::query(
                            "SELECT DISTINCT ON (consent_type)
                                    consent_type, granted, document_version,
                                    granted_at AS updated_at
                             FROM user_consent WHERE user_id = $1
                             ORDER BY consent_type, event_sequence DESC",
                        )
                        .bind(target_id)
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .into_iter()
                        .map(map_consent)
                        .collect::<Result<Vec<_>, _>>()?;
                        let presence = sqlx::query(
                            "SELECT is_location_fresh, updated_at
                             FROM user_presence WHERE user_id = $1",
                        )
                        .bind(target_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .map(|row| {
                            let updated_at: DateTime<Utc> =
                                row.try_get("updated_at").map_err(map_sqlx_error)?;
                            Ok(AdminPresence {
                                is_location_fresh: row
                                    .try_get("is_location_fresh")
                                    .map_err(map_sqlx_error)?,
                                updated_at: super::domain::wire_timestamp(updated_at),
                            })
                        })
                        .transpose()?;
                        record_audit(
                            connection,
                            target_id,
                            admin_id,
                            admin_role,
                            "view_profile",
                            &reason,
                        )
                        .await?;
                        Ok(Some(AdminUserDetailRow {
                            user,
                            banned_reason,
                            preferences,
                            traits,
                            consents,
                            presence,
                        }))
                    })
                })
                .await
        })
    }

    fn set_ban(
        &self,
        target_id: Uuid,
        is_banned: bool,
        reason: String,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> AdministrationStoreFuture<'_, BanResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let role: Option<String> = sqlx::query_scalar(
                            "SELECT role FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL FOR UPDATE",
                        )
                        .bind(target_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some(role) = role else {
                            return Ok(BanResult::NotFound);
                        };
                        let role = AdminUserRole::parse(&role).ok_or(DatabaseError::QueryFailed)?;
                        if target_id == admin_id
                            || role == AdminUserRole::Superadmin
                            || (admin_role == AdminRole::Admin && role != AdminUserRole::User)
                        {
                            return Ok(BanResult::Forbidden);
                        }
                        sqlx::query(
                            "UPDATE user_account SET is_banned = $2,
                               banned_at = CASE WHEN $2 THEN clock_timestamp() ELSE NULL END,
                               banned_reason = CASE WHEN $2 THEN $3 ELSE NULL END,
                               banned_by = CASE WHEN $2 THEN $4::uuid ELSE NULL END
                             WHERE user_id = $1",
                        )
                        .bind(target_id)
                        .bind(is_banned)
                        .bind(&reason)
                        .bind(admin_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if is_banned {
                            revoke_mobile_sessions(connection, target_id).await?;
                        }
                        record_audit(
                            connection,
                            target_id,
                            admin_id,
                            admin_role,
                            if is_banned {
                                "admin_ban"
                            } else {
                                "admin_unban"
                            },
                            &reason,
                        )
                        .await?;
                        Ok(BanResult::Updated)
                    })
                })
                .await
        })
    }

    fn matches(
        &self,
        user_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMatchRow>>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let exists: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL)",
                        )
                        .bind(user_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if !exists {
                            return Ok(None);
                        }
                        let rows = sqlx::query(LIST_MATCHES_SQL)
                            .bind(user_id)
                            .bind(i64::from(limit))
                            .bind(i64::from(offset))
                            .bind(cursor.as_ref().map(|value| value.at))
                            .bind(cursor.as_ref().map(|value| value.id))
                            .fetch_all(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?
                            .into_iter()
                            .map(map_match)
                            .collect::<Result<Vec<_>, _>>()?;
                        record_audit(
                            connection,
                            user_id,
                            admin_id,
                            admin_role,
                            "view_matches",
                            &reason,
                        )
                        .await?;
                        Ok(Some(rows))
                    })
                })
                .await
        })
    }

    fn messages(
        &self,
        match_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMessageRow>>> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let participants =
                            sqlx::query("SELECT user1_id, user2_id FROM match_init WHERE id = $1")
                                .bind(match_id)
                                .fetch_optional(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                        let Some(participants) = participants else {
                            return Ok(None);
                        };
                        let user1_id: Uuid =
                            participants.try_get("user1_id").map_err(map_sqlx_error)?;
                        let user2_id: Uuid =
                            participants.try_get("user2_id").map_err(map_sqlx_error)?;
                        record_audit(
                            connection,
                            user1_id,
                            admin_id,
                            admin_role,
                            "view_messages",
                            &reason,
                        )
                        .await?;
                        record_audit(
                            connection,
                            user2_id,
                            admin_id,
                            admin_role,
                            "view_messages",
                            &reason,
                        )
                        .await?;
                        let rows = sqlx::query(
                            "SELECT id, match_id, sender_id, content, created_at, read_at,
                               to_char(created_at AT TIME ZONE 'UTC',
                                 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                             FROM chat_message WHERE match_id = $1
                               AND ($4::timestamptz IS NULL OR
                                    (created_at, id) < ($4::timestamptz, $5::uuid))
                             ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
                        )
                        .bind(match_id)
                        .bind(i64::from(limit))
                        .bind(i64::from(offset))
                        .bind(cursor.as_ref().map(|value| value.at))
                        .bind(cursor.as_ref().map(|value| value.id))
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .into_iter()
                        .map(map_message)
                        .collect::<Result<Vec<_>, _>>()?;
                        Ok(Some(rows))
                    })
                })
                .await
        })
    }
}

fn map_admin_user(row: sqlx::postgres::PgRow) -> Result<AdminUserRow, DatabaseError> {
    let role: String = row.try_get("role").map_err(map_sqlx_error)?;
    let sex = row
        .try_get::<Option<String>, _>("sex")
        .map_err(map_sqlx_error)?
        .map(|value| Sex::parse(&value).ok_or(DatabaseError::QueryFailed))
        .transpose()?;
    Ok(AdminUserRow {
        id: row.try_get("id").map_err(map_sqlx_error)?,
        role: AdminUserRole::parse(&role).ok_or(DatabaseError::QueryFailed)?,
        is_banned: row.try_get("is_banned").map_err(map_sqlx_error)?,
        banned_at: row.try_get("banned_at").map_err(map_sqlx_error)?,
        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
        firstname: row.try_get("firstname").map_err(map_sqlx_error)?,
        birthdate: row
            .try_get::<Option<NaiveDate>, _>("birthdate")
            .map_err(map_sqlx_error)?,
        sex,
        photo_object_key: row.try_get("photo").map_err(map_sqlx_error)?,
        plan: row.try_get("plan").map_err(map_sqlx_error)?,
        onboarding_complete: row.try_get("onboarding_complete").map_err(map_sqlx_error)?,
        reports_received: row.try_get("reports_received").map_err(map_sqlx_error)?,
        matches_count: row.try_get("matches_count").map_err(map_sqlx_error)?,
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn map_preferences(row: sqlx::postgres::PgRow) -> Result<AdminPreferences, DatabaseError> {
    let looking_for: String = row.try_get("looking_for").map_err(map_sqlx_error)?;
    Ok(AdminPreferences {
        min_age: row.try_get("min_age").map_err(map_sqlx_error)?,
        max_age: row.try_get("max_age").map_err(map_sqlx_error)?,
        max_distance_km: row.try_get("max_distance_km").map_err(map_sqlx_error)?,
        looking_for: LookingFor::parse(&looking_for).ok_or(DatabaseError::QueryFailed)?,
    })
}

fn map_consent(row: sqlx::postgres::PgRow) -> Result<AdminConsent, DatabaseError> {
    let consent_type: String = row.try_get("consent_type").map_err(map_sqlx_error)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(map_sqlx_error)?;
    Ok(AdminConsent {
        consent_type: ConsentType::parse(&consent_type).ok_or(DatabaseError::QueryFailed)?,
        granted: row.try_get("granted").map_err(map_sqlx_error)?,
        document_version: row.try_get("document_version").map_err(map_sqlx_error)?,
        updated_at: super::domain::wire_timestamp(updated_at),
    })
}

fn map_match(row: sqlx::postgres::PgRow) -> Result<CursorMatchRow, DatabaseError> {
    let status: String = row.try_get("status").map_err(map_sqlx_error)?;
    Ok(CursorMatchRow {
        item: MatchRecord {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            user1_id: row.try_get("user1_id").map_err(map_sqlx_error)?,
            user2_id: row.try_get("user2_id").map_err(map_sqlx_error)?,
            status: MatchStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?,
            expires_at: row.try_get("expires_at").map_err(map_sqlx_error)?,
            purge_after: row.try_get("purge_after").map_err(map_sqlx_error)?,
            continuation_initiator_id: row
                .try_get("continuation_initiator_id")
                .map_err(map_sqlx_error)?,
            created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
            last_message_at: row.try_get("last_message_at").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn map_message(row: sqlx::postgres::PgRow) -> Result<CursorMessageRow, DatabaseError> {
    Ok(CursorMessageRow {
        item: MessageRecord {
            id: row.try_get("id").map_err(map_sqlx_error)?,
            match_id: row.try_get("match_id").map_err(map_sqlx_error)?,
            sender_id: row.try_get("sender_id").map_err(map_sqlx_error)?,
            content: row.try_get("content").map_err(map_sqlx_error)?,
            created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
            read_at: row.try_get("read_at").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

async fn revoke_mobile_sessions(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "UPDATE refresh_token_family
         SET revoked_at = clock_timestamp(), revocation_reason = 'banned'
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1 AND revoked = false")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM device_token WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn record_audit(
    connection: &mut sqlx::PgConnection,
    target_id: Uuid,
    admin_id: Uuid,
    admin_role: AdminRole,
    action: &'static str,
    reason: &str,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO data_access_log
         (accessed_user_id, accessor_id, accessor_role, action, reason)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(target_id)
    .bind(admin_id)
    .bind(admin_role.as_str())
    .bind(action)
    .bind(reason)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

fn canonical_uuid(value: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(value).ok()?;
    (value.len() == 36
        && id.hyphenated().to_string().eq_ignore_ascii_case(value)
        && (1..=5).contains(&id.get_version_num()))
    .then_some(id)
}

const LIST_USERS_SQL: &str = r#"
SELECT account.user_id AS id, account.role, account.is_banned, account.banned_at,
  account.created_at, profile.firstname, profile.birthdate, profile.sex,
  NULL::text AS photo, COALESCE(subscription.plan, 'free') AS plan,
  (account.role <> 'user' OR (
    EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'terms_of_service_acceptance' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $8)
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'privacy_notice_acknowledgement' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $9)
  )) AS onboarding_complete,
  (SELECT count(*)::int FROM user_report WHERE reported_id = account.user_id) AS reports_received,
  (SELECT count(*)::int FROM match_init WHERE user1_id = account.user_id OR user2_id = account.user_id) AS matches_count,
  to_char(account.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM user_account AS account
LEFT JOIN user_profile AS profile ON profile.user_id = account.user_id
LEFT JOIN user_subscription AS subscription ON subscription.user_id = account.user_id
WHERE account.deleted_at IS NULL
  AND ($1::text IS NULL OR ($1 = 'banned' AND account.is_banned)
    OR ($1 = 'active' AND NOT account.is_banned))
  AND ($2::text IS NULL OR account.role = $2)
  AND ($3 = '' OR profile.firstname ILIKE '%' || $3 || '%' OR account.user_id = $10::uuid)
  AND ($6::timestamptz IS NULL OR
       (account.created_at, account.user_id) < ($6::timestamptz, $7::uuid))
ORDER BY account.created_at DESC, account.user_id DESC LIMIT $4 OFFSET $5
"#;

const DETAIL_USER_SQL: &str = r#"
SELECT account.user_id AS id, account.role, account.is_banned, account.banned_at,
  account.banned_reason, account.created_at, profile.firstname, profile.birthdate,
  profile.sex, photo.object_key AS photo, COALESCE(subscription.plan, 'free') AS plan,
  (account.role <> 'user' OR (
    EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'terms_of_service_acceptance' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $2)
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'privacy_notice_acknowledgement' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $3)
  )) AS onboarding_complete,
  (SELECT count(*)::int FROM user_report WHERE reported_id = account.user_id) AS reports_received,
  (SELECT count(*)::int FROM match_init WHERE user1_id = account.user_id OR user2_id = account.user_id) AS matches_count,
  to_char(account.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM user_account AS account
LEFT JOIN user_profile AS profile ON profile.user_id = account.user_id
LEFT JOIN user_photo AS photo ON photo.user_id = profile.user_id AND photo.status = 'ready'
LEFT JOIN user_subscription AS subscription ON subscription.user_id = account.user_id
WHERE account.user_id = $1 AND account.deleted_at IS NULL
"#;

const LIST_MATCHES_SQL: &str = r#"
WITH page AS MATERIALIZED (
  SELECT match_record.*,
    COALESCE(match_record.last_message_at, match_record.created_at) AS activity_at
  FROM match_init AS match_record
  WHERE (match_record.user1_id = $1 OR match_record.user2_id = $1)
    AND ($4::timestamptz IS NULL OR
      (COALESCE(match_record.last_message_at, match_record.created_at), match_record.id)
        < ($4::timestamptz, $5::uuid))
  ORDER BY activity_at DESC, match_record.id DESC LIMIT $2 OFFSET $3
)
SELECT id, user1_id, user2_id, status, expires_at, purge_after,
  continuation_initiator_id, created_at, last_message_at,
  to_char(activity_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM page ORDER BY activity_at DESC, id DESC
"#;
