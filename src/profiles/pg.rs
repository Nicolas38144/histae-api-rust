use std::future::Future;
use std::pin::Pin;

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use super::domain::{
    ConsentRecord, ConsentType, LookingFor, ModerationReason, ModerationStatus, Preferences,
    PreferencesInput, PresenceInput, ProfileInput, ProfileRecord, Sex, VersionedConsentChange,
    WriteOutcome,
};
use crate::config::LegalConfig;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};

pub type ProfileStoreFuture<'store, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'store>>;

pub trait ProfileStore: Send + Sync {
    fn find_profile(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Option<ProfileRecord>>;
    fn upsert_profile(
        &self,
        user_id: Uuid,
        input: ProfileInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome>;
    fn find_preferences(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Option<Preferences>>;
    fn upsert_preferences(
        &self,
        user_id: Uuid,
        input: PreferencesInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome>;
    fn upsert_presence(
        &self,
        user_id: Uuid,
        input: PresenceInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome>;
    fn current_consents(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Vec<ConsentRecord>>;
    fn record_consents(
        &self,
        user_id: Uuid,
        changes: Vec<VersionedConsentChange>,
        ip_address: String,
        user_agent: String,
    ) -> ProfileStoreFuture<'_, bool>;
}

#[derive(Clone)]
pub struct PgProfileRepository {
    database: Database,
}

impl PgProfileRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl ProfileStore for PgProfileRepository {
    fn find_profile(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Option<ProfileRecord>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                r#"
                SELECT profile.user_id, profile.firstname, profile.birthdate, profile.sex,
                    profile.bio, photo.object_key AS photo,
                    bio_moderation.status AS bio_moderation_status,
                    bio_moderation.reason_codes AS bio_moderation_reasons,
                    photo_moderation.status AS photo_moderation_status,
                    photo_moderation.reason_codes AS photo_moderation_reasons,
                    COALESCE(answers.items, '[]'::jsonb) AS profile_answers
                FROM user_profile AS profile
                JOIN user_account AS account ON account.user_id = profile.user_id
                LEFT JOIN user_photo AS photo
                    ON photo.user_id = profile.user_id AND photo.status = 'ready'
                LEFT JOIN content_moderation_case AS bio_moderation
                    ON bio_moderation.bio_user_id = profile.user_id
                LEFT JOIN content_moderation_case AS photo_moderation
                    ON photo_moderation.photo_id = photo.id
                LEFT JOIN LATERAL (
                    SELECT jsonb_agg(jsonb_build_object(
                        'question_id', answer.question_id,
                        'code', question.code,
                        'question', question.prompt,
                        'answer', answer.answer,
                        'position', answer.position,
                        'moderation_status', moderation.status,
                        'moderation_reasons', moderation.reason_codes
                    ) ORDER BY answer.position) AS items
                    FROM user_profile_answer AS answer
                    JOIN profile_question AS question ON question.id = answer.question_id
                    JOIN content_moderation_case AS moderation
                        ON moderation.profile_answer_id = answer.id
                    WHERE answer.user_id = profile.user_id
                ) AS answers ON true
                WHERE profile.user_id = $1 AND account.deleted_at IS NULL
                "#,
            )
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(profile_from_row).transpose()
        })
    }

    fn upsert_profile(
        &self,
        user_id: Uuid,
        input: ProfileInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            self.database
                .transaction(move |connection| {
                    Box::pin(async move {
                        let sensitive = input.sex.map(|_| ConsentType::SensitiveDataConsent);
                        let lock = lock_legal_choices(connection, user_id, sensitive, &legal).await?;
                        if lock != WriteOutcome::Updated {
                            return Ok(lock);
                        }
                        let current_bio: Option<String> = sqlx::query_scalar(
                            "SELECT bio FROM user_profile WHERE user_id = $1 FOR UPDATE",
                        )
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .flatten();
                        let result = sqlx::query(
                            r#"
                            INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
                            VALUES ($1, $2, $3, $4, $5)
                            ON CONFLICT (user_id) DO UPDATE SET
                                firstname = EXCLUDED.firstname,
                                birthdate = EXCLUDED.birthdate,
                                sex = EXCLUDED.sex,
                                bio = EXCLUDED.bio
                            "#,
                        )
                        .bind(user_id)
                        .bind(&input.firstname)
                        .bind(input.birthdate)
                        .bind(input.sex.map(Sex::as_str))
                        .bind(&input.bio)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if result.rows_affected() == 0 {
                            return Ok(WriteOutcome::AccountNotFound);
                        }
                        match input.bio.as_deref() {
                            None | Some("") => {
                                sqlx::query(
                                    "DELETE FROM content_moderation_case WHERE bio_user_id = $1",
                                )
                                .bind(user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            }
                            Some(bio) if current_bio.as_deref() != Some(bio) => {
                                let decision = input
                                    .bio_moderation
                                    .ok_or(DatabaseError::QueryFailed)?;
                                let reasons = decision
                                    .reasons
                                    .into_iter()
                                    .map(ModerationReason::as_str)
                                    .collect::<Vec<_>>();
                                sqlx::query(
                                    r#"
                                    INSERT INTO content_moderation_case (
                                        user_id, content_type, bio_user_id, status,
                                        reason_codes, policy_version
                                    ) VALUES ($1, 'bio', $1, $2, $3, $4)
                                    ON CONFLICT (bio_user_id) WHERE bio_user_id IS NOT NULL DO UPDATE
                                    SET status = EXCLUDED.status,
                                        reason_codes = EXCLUDED.reason_codes,
                                        policy_version = EXCLUDED.policy_version,
                                        version = content_moderation_case.version + 1,
                                        reviewed_by = NULL,
                                        reviewed_at = NULL,
                                        review_reason = NULL,
                                        updated_at = clock_timestamp()
                                    "#,
                                )
                                .bind(user_id)
                                .bind(decision.status.as_str())
                                .bind(reasons)
                                .bind(decision.policy_version)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                            }
                            Some(_) => {}
                        }
                        Ok(WriteOutcome::Updated)
                    })
                })
                .await
        })
    }

    fn find_preferences(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Option<Preferences>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                r#"
                SELECT preferences.user_id, preferences.min_age, preferences.max_age,
                    preferences.max_distance_km, preferences.looking_for
                FROM user_preferences AS preferences
                JOIN user_account AS account ON account.user_id = preferences.user_id
                WHERE preferences.user_id = $1 AND account.deleted_at IS NULL
                "#,
            )
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                let looking_for: String = row.try_get("looking_for").map_err(map_sqlx_error)?;
                Ok(Preferences {
                    user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                    min_age: row.try_get("min_age").map_err(map_sqlx_error)?,
                    max_age: row.try_get("max_age").map_err(map_sqlx_error)?,
                    max_distance_km: row.try_get("max_distance_km").map_err(map_sqlx_error)?,
                    looking_for: LookingFor::parse(&looking_for)
                        .ok_or(DatabaseError::QueryFailed)?,
                })
            })
            .transpose()
        })
    }

    fn upsert_preferences(
        &self,
        user_id: Uuid,
        input: PreferencesInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            self.database
                .transaction(move |connection| {
                    Box::pin(async move {
                        let lock = lock_legal_choices(
                            connection,
                            user_id,
                            Some(ConsentType::SensitiveDataConsent),
                            &legal,
                        )
                        .await?;
                        if lock != WriteOutcome::Updated {
                            return Ok(lock);
                        }
                        sqlx::query(
                            r#"
                            INSERT INTO user_preferences (
                                user_id, min_age, max_age, max_distance_km, looking_for
                            ) VALUES ($1, $2, $3, $4, $5)
                            ON CONFLICT (user_id) DO UPDATE SET
                                min_age = EXCLUDED.min_age,
                                max_age = EXCLUDED.max_age,
                                max_distance_km = EXCLUDED.max_distance_km,
                                looking_for = EXCLUDED.looking_for
                            "#,
                        )
                        .bind(user_id)
                        .bind(input.min_age)
                        .bind(input.max_age)
                        .bind(input.max_distance_km)
                        .bind(input.looking_for.as_str())
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(WriteOutcome::Updated)
                    })
                })
                .await
        })
    }

    fn upsert_presence(
        &self,
        user_id: Uuid,
        input: PresenceInput,
        legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            self.database
                .transaction(move |connection| {
                    Box::pin(async move {
                        let lock = lock_legal_choices(
                            connection,
                            user_id,
                            Some(ConsentType::LocationConsent),
                            &legal,
                        )
                        .await?;
                        if lock != WriteOutcome::Updated {
                            return Ok(lock);
                        }
                        sqlx::query(
                            r#"
                            INSERT INTO user_presence (
                                user_id, latitude, longitude, is_location_fresh, updated_at
                            ) VALUES ($1, ($2::double precision)::numeric,
                                ($3::double precision)::numeric, true, $4)
                            ON CONFLICT (user_id) DO UPDATE SET
                                latitude = EXCLUDED.latitude,
                                longitude = EXCLUDED.longitude,
                                is_location_fresh = EXCLUDED.is_location_fresh,
                                updated_at = EXCLUDED.updated_at
                            "#,
                        )
                        .bind(user_id)
                        .bind(input.latitude)
                        .bind(input.longitude)
                        .bind(input.updated_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(WriteOutcome::Updated)
                    })
                })
                .await
        })
    }

    fn current_consents(&self, user_id: Uuid) -> ProfileStoreFuture<'_, Vec<ConsentRecord>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let rows = sqlx::query(
                r#"
                SELECT DISTINCT ON (consent_type)
                    consent_type, granted, document_version, granted_at
                FROM user_consent
                WHERE user_id = $1
                ORDER BY consent_type, event_sequence DESC
                "#,
            )
            .bind(user_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|row| {
                    let consent_type: String =
                        row.try_get("consent_type").map_err(map_sqlx_error)?;
                    Ok(ConsentRecord {
                        consent_type: ConsentType::parse(&consent_type)
                            .ok_or(DatabaseError::QueryFailed)?,
                        granted: row.try_get("granted").map_err(map_sqlx_error)?,
                        document_version: row
                            .try_get("document_version")
                            .map_err(map_sqlx_error)?,
                        granted_at: row.try_get("granted_at").map_err(map_sqlx_error)?,
                    })
                })
                .collect()
        })
    }

    fn record_consents(
        &self,
        user_id: Uuid,
        changes: Vec<VersionedConsentChange>,
        ip_address: String,
        user_agent: String,
    ) -> ProfileStoreFuture<'_, bool> {
        Box::pin(async move {
            self.database
                .transaction(move |connection| {
                    Box::pin(async move {
                        let account: Option<Uuid> = sqlx::query_scalar(
                            r#"
                            SELECT user_id FROM user_account
                            WHERE user_id = $1 AND deleted_at IS NULL
                            FOR UPDATE
                            "#,
                        )
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if account.is_none() {
                            return Ok(false);
                        }
                        for change in changes {
                            let current = sqlx::query(
                                r#"
                                SELECT granted, document_version
                                FROM user_consent
                                WHERE user_id = $1 AND consent_type = $2
                                ORDER BY event_sequence DESC
                                LIMIT 1
                                "#,
                            )
                            .bind(user_id)
                            .bind(change.consent_type.as_str())
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            if current.as_ref().is_some_and(|row| {
                                row.try_get::<bool, _>("granted").ok() == Some(change.granted)
                                    && row.try_get::<String, _>("document_version").ok()
                                        == Some(change.document_version.clone())
                            }) {
                                continue;
                            }
                            sqlx::query(
                                r#"
                                UPDATE user_consent SET withdrawn_at = clock_timestamp()
                                WHERE user_id = $1 AND consent_type = $2
                                    AND granted = true AND withdrawn_at IS NULL
                                "#,
                            )
                            .bind(user_id)
                            .bind(change.consent_type.as_str())
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                            sqlx::query(
                                r#"
                                INSERT INTO user_consent (
                                    user_id, consent_type, granted, document_version,
                                    ip_address, user_agent, granted_at, withdrawn_at
                                ) VALUES (
                                    $1, $2, $3, $4, $5, $6, clock_timestamp(),
                                    CASE WHEN $3 THEN NULL ELSE clock_timestamp() END
                                )
                                "#,
                            )
                            .bind(user_id)
                            .bind(change.consent_type.as_str())
                            .bind(change.granted)
                            .bind(&change.document_version)
                            .bind((!ip_address.is_empty()).then_some(ip_address.as_str()))
                            .bind((!user_agent.is_empty()).then_some(user_agent.as_str()))
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;

                            if !change.granted
                                && change.consent_type == ConsentType::SensitiveDataConsent
                            {
                                sqlx::query(
                                    "UPDATE user_profile SET sex = NULL WHERE user_id = $1",
                                )
                                .bind(user_id)
                                .execute(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?;
                                sqlx::query("DELETE FROM user_preferences WHERE user_id = $1")
                                    .bind(user_id)
                                    .execute(&mut *connection)
                                    .await
                                    .map_err(map_sqlx_error)?;
                            }
                            if !change.granted
                                && change.consent_type == ConsentType::LocationConsent
                            {
                                sqlx::query("DELETE FROM user_presence WHERE user_id = $1")
                                    .bind(user_id)
                                    .execute(&mut *connection)
                                    .await
                                    .map_err(map_sqlx_error)?;
                            }
                        }
                        Ok(true)
                    })
                })
                .await
        })
    }
}

async fn lock_legal_choices(
    connection: &mut PgConnection,
    user_id: Uuid,
    sensitive: Option<ConsentType>,
    legal: &LegalConfig,
) -> Result<WriteOutcome, DatabaseError> {
    let account: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT user_id FROM user_account
        WHERE user_id = $1 AND deleted_at IS NULL
        FOR UPDATE
        "#,
    )
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if account.is_none() {
        return Ok(WriteOutcome::AccountNotFound);
    }
    let mut required = ConsentType::ONBOARDING.to_vec();
    if let Some(sensitive) = sensitive {
        required.push(sensitive);
    }
    let names = required
        .iter()
        .map(|consent| consent.as_str())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        SELECT consent_type, document_version FROM user_consent
        WHERE user_id = $1 AND consent_type = ANY($2::text[])
            AND granted = true AND withdrawn_at IS NULL
        "#,
    )
    .bind(user_id)
    .bind(names)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    for required_type in required {
        let present = rows.iter().any(|row| {
            row.try_get::<String, _>("consent_type").ok().as_deref() == Some(required_type.as_str())
                && row.try_get::<String, _>("document_version").ok().as_deref()
                    == Some(required_type.version(legal))
        });
        if !present {
            return Ok(WriteOutcome::RequiredConsentMissing);
        }
    }
    Ok(WriteOutcome::Updated)
}

fn profile_from_row(row: sqlx::postgres::PgRow) -> Result<ProfileRecord, DatabaseError> {
    let sex = row
        .try_get::<Option<String>, _>("sex")
        .map_err(map_sqlx_error)?
        .map(|value| Sex::parse(&value).ok_or(DatabaseError::QueryFailed))
        .transpose()?;
    let answers: Value = row.try_get("profile_answers").map_err(map_sqlx_error)?;
    let profile_answers = answers
        .as_array()
        .cloned()
        .ok_or(DatabaseError::QueryFailed)?;
    Ok(ProfileRecord {
        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
        firstname: row.try_get("firstname").map_err(map_sqlx_error)?,
        birthdate: row
            .try_get::<NaiveDate, _>("birthdate")
            .map_err(map_sqlx_error)?,
        sex,
        bio: row.try_get("bio").map_err(map_sqlx_error)?,
        photo_object_key: row.try_get("photo").map_err(map_sqlx_error)?,
        profile_answers,
        bio_moderation_status: parse_optional_status(
            row.try_get("bio_moderation_status")
                .map_err(map_sqlx_error)?,
        )?,
        bio_moderation_reasons: parse_reasons(
            row.try_get("bio_moderation_reasons")
                .map_err(map_sqlx_error)?,
        )?,
        photo_moderation_status: parse_optional_status(
            row.try_get("photo_moderation_status")
                .map_err(map_sqlx_error)?,
        )?,
        photo_moderation_reasons: parse_reasons(
            row.try_get("photo_moderation_reasons")
                .map_err(map_sqlx_error)?,
        )?,
    })
}

fn parse_optional_status(value: Option<String>) -> Result<Option<ModerationStatus>, DatabaseError> {
    value
        .map(|value| ModerationStatus::parse(&value).ok_or(DatabaseError::QueryFailed))
        .transpose()
}

fn parse_reasons(values: Option<Vec<String>>) -> Result<Vec<ModerationReason>, DatabaseError> {
    values
        .unwrap_or_default()
        .into_iter()
        .map(|value| ModerationReason::parse(&value).ok_or(DatabaseError::QueryFailed))
        .collect()
}
