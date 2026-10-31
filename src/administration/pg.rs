mod conversations;
mod premium;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{Postgres, QueryBuilder, Row as _, types::Json};
use uuid::Uuid;

use super::domain::{
    AdminConsent, AdminPreferences, AdminPresence, AdminTrait, AdminUserDetailRow, AdminUserRole,
    AdminUserRow, AdminUserStatus, BanResult, CursorMatchRow, CursorMessageRow, PageCursor,
    PremiumGrantResult, RoleChangeResult,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
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
    fn set_complimentary_premium(
        &self,
        target_id: Uuid,
        enabled: bool,
        reason: String,
        actor_id: Uuid,
    ) -> AdministrationStoreFuture<'_, PremiumGrantResult> {
        Box::pin(self.set_premium_grant(target_id, enabled, reason, actor_id))
    }

    fn user_names(
        &self,
        ids: Vec<Uuid>,
    ) -> AdministrationStoreFuture<'_, Vec<(Uuid, Option<String>)>> {
        Box::pin(async move {
            sqlx::query_as(
                "SELECT account.user_id, profile.firstname FROM user_account AS account
                 LEFT JOIN user_profile AS profile USING (user_id)
                 WHERE account.user_id = ANY($1) AND account.deleted_at IS NULL",
            )
            .bind(ids)
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)
        })
    }

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
            let mut query = QueryBuilder::<Postgres>::new(
                "WITH page AS MATERIALIZED (
                   SELECT account.user_id, account.role, account.is_banned, account.banned_at,
                     account.created_at, profile.firstname, profile.birthdate, profile.sex
                   FROM user_account account
                   LEFT JOIN user_profile profile ON profile.user_id = account.user_id
                   WHERE account.deleted_at IS NULL",
            );
            if let Some(status) = status {
                query.push(match status {
                    AdminUserStatus::Active => " AND NOT account.is_banned",
                    AdminUserStatus::Banned => " AND account.is_banned",
                });
            }
            if let Some(role) = role {
                query.push(" AND account.role = ").push_bind(role.as_str());
            }
            if !search.is_empty() {
                // Search each indexed source independently instead of an OR across a join.
                query.push(" AND account.user_id IN (SELECT user_id FROM user_profile WHERE firstname ILIKE ")
                    .push_bind(format!("%{search}%"));
                if let Some(id) = searched_id {
                    query.push(" UNION SELECT ").push_bind(id);
                }
                query.push(")");
            }
            if let Some(cursor) = cursor {
                query
                    .push(" AND (account.created_at, account.user_id) < (")
                    .push_bind(cursor.at)
                    .push(", ")
                    .push_bind(cursor.id)
                    .push(")");
            }
            query.push(" ORDER BY account.created_at DESC, account.user_id DESC LIMIT ")
                .push_bind(i64::from(limit)).push(" OFFSET ").push_bind(i64::from(offset))
                .push(") SELECT account.user_id AS id, account.role, account.is_banned, account.banned_at,
                  account.created_at, account.firstname, account.birthdate, account.sex,
                  NULL::text AS photo, COALESCE(subscription.plan, 'free') AS plan,
                  (account.role <> 'user' OR (
                    EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
                      AND consent_type = 'terms_of_service_acceptance' AND granted
                      AND withdrawn_at IS NULL AND document_version = ")
                .push_bind(terms_version)
                .push(") AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
                      AND consent_type = 'privacy_notice_acknowledgement' AND granted
                      AND withdrawn_at IS NULL AND document_version = ")
                .push_bind(privacy_version)
                .push("))) AS onboarding_complete,
                  (SELECT count(*)::int FROM user_report WHERE reported_id = account.user_id) AS reports_received,
                  ((SELECT count(*) FROM match_init WHERE user1_id = account.user_id)
                    + (SELECT count(*) FROM match_init WHERE user2_id = account.user_id))::int AS matches_count,
                  to_char(account.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                  FROM page account
                  LEFT JOIN effective_user_subscription subscription ON subscription.user_id = account.user_id AND (subscription.plan='free' OR subscription.access_granted)
                  ORDER BY account.created_at DESC, account.user_id DESC");
            let rows = query
                .build()
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
                        let complimentary_premium = account
                            .try_get("complimentary_premium")
                            .map_err(map_sqlx_error)?;
                        let is_review_account = account
                            .try_get("is_review_account")
                            .map_err(map_sqlx_error)?;
                        let preferences = account
                            .try_get::<Option<Uuid>, _>("preferences_user_id")
                            .map_err(map_sqlx_error)?
                            .map(|_| map_preferences(&account))
                            .transpose()?;
                        let traits: Json<Vec<(Uuid, String)>> =
                            account.try_get("traits").map_err(map_sqlx_error)?;
                        let traits = traits
                            .0
                            .into_iter()
                            .map(|(id, name)| AdminTrait { id, name })
                            .collect();
                        type ConsentTuple = (String, bool, String, DateTime<Utc>);
                        let consents: Json<Vec<ConsentTuple>> =
                            account.try_get("consents").map_err(map_sqlx_error)?;
                        let consents = consents
                            .0
                            .into_iter()
                            .map(|(kind, granted, document_version, updated_at)| {
                                Ok(AdminConsent {
                                    consent_type: ConsentType::parse(&kind)
                                        .ok_or(DatabaseError::QueryFailed)?,
                                    granted,
                                    document_version,
                                    updated_at: super::domain::wire_timestamp(updated_at),
                                })
                            })
                            .collect::<Result<Vec<_>, DatabaseError>>()?;
                        let presence = account
                            .try_get::<Option<DateTime<Utc>>, _>("presence_updated_at")
                            .map_err(map_sqlx_error)?
                            .map(|updated_at| {
                                Ok(AdminPresence {
                                    is_location_fresh: account
                                        .try_get("is_location_fresh")
                                        .map_err(map_sqlx_error)?,
                                    updated_at: super::domain::wire_timestamp(updated_at),
                                })
                            })
                            .transpose()?;
                        let user = map_admin_user(account)?;
                        record_audit(
                            connection,
                            target_id,
                            admin_id,
                            admin_role,
                            "view_profile",
                            &reason,
                        )
                        .await?;
                        let now = sqlx::query_scalar("SELECT clock_timestamp()")
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        let quota =
                            crate::matches::pg::credits::quota_on(connection, target_id, now)
                                .await
                                .map_err(|error| match error {
                                    crate::matches::store::MatchStoreError::Database(error) => {
                                        error
                                    }
                                    _ => DatabaseError::QueryFailed,
                                })?;
                        Ok(Some(AdminUserDetailRow {
                            user,
                            complimentary_premium,
                            is_review_account,
                            banned_reason,
                            preferences,
                            traits,
                            consents,
                            presence,
                            continuation_quota: Some(quota),
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

    fn set_role(
        &self,
        target_id: Uuid,
        role: AdminUserRole,
        reason: String,
        actor_id: Uuid,
    ) -> AdministrationStoreFuture<'_, RoleChangeResult> {
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        let actor: Option<Uuid> = sqlx::query_scalar(
                            "SELECT user_id FROM user_account WHERE user_id = $1
                             AND role = 'superadmin' AND deleted_at IS NULL
                             AND is_banned = false FOR UPDATE",
                        )
                        .bind(actor_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if actor.is_none() || target_id == actor_id {
                            return Ok(RoleChangeResult::Forbidden);
                        }
                        let target: Option<(String, bool, bool)> = sqlx::query_as(
                            "SELECT role, is_banned, is_review_account FROM user_account
                             WHERE user_id = $1 AND deleted_at IS NULL FOR UPDATE",
                        )
                        .bind(target_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some((current_role_raw, is_banned, is_review)) = target else {
                            return Ok(RoleChangeResult::NotFound);
                        };
                        let Some(current_role) = AdminUserRole::parse(&current_role_raw) else {
                            return Err(DatabaseError::QueryFailed);
                        };
                        if current_role == AdminUserRole::Superadmin
                            || role == AdminUserRole::Superadmin
                            || (role == AdminUserRole::Admin && is_banned)
                            || (role != AdminUserRole::User && is_review)
                        {
                            return Ok(RoleChangeResult::Forbidden);
                        }
                        if current_role == role {
                            return Ok(RoleChangeResult::Unchanged);
                        }
                        sqlx::query("UPDATE user_account SET role = $2 WHERE user_id = $1")
                            .bind(target_id)
                            .bind(role.as_str())
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        if role == AdminUserRole::User {
                            revoke_admin_access(connection, target_id).await?;
                        }
                        record_audit(
                            connection,
                            target_id,
                            actor_id,
                            AdminRole::Superadmin,
                            if role == AdminUserRole::Admin {
                                "admin_promote"
                            } else {
                                "admin_demote"
                            },
                            &reason,
                        )
                        .await?;
                        Ok(RoleChangeResult::Updated)
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
        Box::pin(
            self.list_conversations(user_id, admin_id, admin_role, reason, limit, offset, cursor),
        )
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
        Box::pin(self.read_conversation(
            match_id, admin_id, admin_role, reason, limit, offset, cursor,
        ))
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

fn map_preferences(row: &sqlx::postgres::PgRow) -> Result<AdminPreferences, DatabaseError> {
    let looking_for: String = row.try_get("looking_for").map_err(map_sqlx_error)?;
    Ok(AdminPreferences {
        min_age: row.try_get("min_age").map_err(map_sqlx_error)?,
        max_age: row.try_get("max_age").map_err(map_sqlx_error)?,
        max_distance_km: row.try_get("max_distance_km").map_err(map_sqlx_error)?,
        looking_for: LookingFor::parse(&looking_for).ok_or(DatabaseError::QueryFailed)?,
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

async fn revoke_admin_access(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
) -> Result<(), DatabaseError> {
    for query in [
        "UPDATE admin_session SET revoked_at = clock_timestamp() WHERE user_id = $1 AND revoked_at IS NULL",
        "UPDATE admin_webauthn_credential SET revoked_at = clock_timestamp() WHERE user_id = $1 AND revoked_at IS NULL",
        "UPDATE admin_webauthn_bootstrap SET consumed_at = clock_timestamp() WHERE user_id = $1 AND consumed_at IS NULL",
        "UPDATE admin_webauthn_challenge SET consumed_at = clock_timestamp() WHERE user_id = $1 AND consumed_at IS NULL",
    ] {
        sqlx::query(query)
            .bind(user_id)
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    }
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

const DETAIL_USER_SQL: &str = r#"
SELECT account.user_id AS id, account.role, account.is_banned, account.banned_at,
  account.is_review_account,
  EXISTS (SELECT 1 FROM complimentary_premium_grant WHERE user_id=account.user_id) AS complimentary_premium,
  account.banned_reason, account.created_at, profile.firstname, profile.birthdate,
  profile.sex, photo.object_key AS photo, COALESCE(subscription.plan, 'free') AS plan,
  preferences.user_id AS preferences_user_id, preferences.min_age, preferences.max_age,
  preferences.max_distance_km, preferences.looking_for,
  presence.is_location_fresh, presence.updated_at AS presence_updated_at,
  COALESCE((SELECT jsonb_agg(jsonb_build_array(trait.id, trait.name) ORDER BY trait.name, trait.id)
    FROM user_trait JOIN trait ON trait.id = user_trait.trait_id
    WHERE user_trait.user_id = account.user_id), '[]'::jsonb) AS traits,
  COALESCE((SELECT jsonb_agg(jsonb_build_array(consent_type, granted, document_version, granted_at) ORDER BY consent_type)
    FROM (SELECT DISTINCT ON (consent_type) consent_type, granted, document_version, granted_at
      FROM user_consent WHERE user_id = account.user_id
      ORDER BY consent_type, event_sequence DESC) latest), '[]'::jsonb) AS consents,
  (account.role <> 'user' OR (
    EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'terms_of_service_acceptance' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $2)
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'privacy_notice_acknowledgement' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $3)
  )) AS onboarding_complete,
  (SELECT count(*)::int FROM user_report WHERE reported_id = account.user_id) AS reports_received,
  ((SELECT count(*) FROM match_init WHERE user1_id = account.user_id)
    + (SELECT count(*) FROM match_init WHERE user2_id = account.user_id))::int AS matches_count,
  to_char(account.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at
FROM user_account AS account
LEFT JOIN user_profile AS profile ON profile.user_id = account.user_id
LEFT JOIN user_preferences AS preferences ON preferences.user_id = account.user_id
LEFT JOIN user_presence AS presence ON presence.user_id = account.user_id
LEFT JOIN user_photo AS photo ON photo.user_id = profile.user_id AND photo.status = 'ready'
LEFT JOIN effective_user_subscription AS subscription ON subscription.user_id = account.user_id AND (subscription.plan='free' OR subscription.access_granted)
WHERE account.user_id = $1 AND account.deleted_at IS NULL
"#;
