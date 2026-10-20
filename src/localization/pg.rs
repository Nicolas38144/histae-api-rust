use super::{
    Language,
    catalog::{CatalogSnapshot, Entry, Translations},
};
use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use sqlx::{Row, types::Json};
use uuid::Uuid;

pub struct PgLocalizationRepository {
    database: crate::infra::postgres::Database,
}
impl PgLocalizationRepository {
    pub fn new(database: crate::infra::postgres::Database) -> Self {
        Self { database }
    }

    pub(super) async fn read_language(&self, user_id: Uuid) -> Result<Language, DatabaseError> {
        let language: Option<String> = sqlx::query_scalar("SELECT preferred_language FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned")
            .bind(user_id).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?;
        language
            .as_deref()
            .and_then(Language::parse)
            .ok_or(DatabaseError::AccountUnavailable)
    }
    pub(super) async fn write_language(
        &self,
        user_id: Uuid,
        language: Language,
    ) -> Result<Language, DatabaseError> {
        let changed: Option<String> = sqlx::query_scalar("UPDATE user_account SET preferred_language=$2 WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned RETURNING preferred_language")
            .bind(user_id).bind(language.as_str()).fetch_optional(self.database.pool()).await.map_err(map_sqlx_error)?;
        changed
            .as_deref()
            .and_then(Language::parse)
            .ok_or(DatabaseError::AccountUnavailable)
    }
    pub(super) async fn read_catalog(&self) -> Result<CatalogSnapshot, DatabaseError> {
        let rows = sqlx::query(r#"
          SELECT 'trait' AS kind, trait.id::text AS key, trait.name AS base,
            COALESCE((SELECT jsonb_object_agg(language,name) FROM trait_translation WHERE trait_id=trait.id),'{}'::jsonb) || jsonb_build_object('fr',trait.name) AS names,
            '{}'::jsonb AS descriptions FROM trait
          UNION ALL SELECT 'question', question.id::text, question.prompt,
            COALESCE((SELECT jsonb_object_agg(language,prompt) FROM profile_question_translation WHERE question_id=question.id),'{}'::jsonb) || jsonb_build_object('fr',question.prompt),
            '{}'::jsonb FROM profile_question AS question
          UNION ALL SELECT 'plan', plan.code, plan.display_name,
            jsonb_build_object('fr',plan.display_name) || COALESCE((SELECT jsonb_object_agg(language,display_name) FROM subscription_plan_translation WHERE plan_code=plan.code),'{}'::jsonb),
            '{}'::jsonb FROM subscription_plan AS plan
          UNION ALL SELECT 'feature', feature.plan_code || ':' || feature.feature_code, COALESCE(feature.display_name,''),
            jsonb_build_object('fr',COALESCE(feature.display_name,'')) || COALESCE((SELECT jsonb_object_agg(language,display_name) FROM subscription_plan_feature_translation WHERE plan_code=feature.plan_code AND feature_code=feature.feature_code),'{}'::jsonb),
            jsonb_build_object('fr',COALESCE(feature.description,'')) || COALESCE((SELECT jsonb_object_agg(language,description) FROM subscription_plan_feature_translation WHERE plan_code=feature.plan_code AND feature_code=feature.feature_code),'{}'::jsonb)
            FROM subscription_plan_feature AS feature
        "#).fetch_all(self.database.pool()).await.map_err(map_sqlx_error)?;
        let mut snapshot = CatalogSnapshot::default();
        for row in rows {
            let key: String = row.try_get("key").map_err(map_sqlx_error)?;
            let names: Json<Translations> = row.try_get("names").map_err(map_sqlx_error)?;
            let descriptions: Json<Translations> =
                row.try_get("descriptions").map_err(map_sqlx_error)?;
            let entry = Entry {
                base: row.try_get("base").map_err(map_sqlx_error)?,
                names: names.0,
                descriptions: descriptions.0,
            };
            match row.try_get::<&str, _>("kind").map_err(map_sqlx_error)? {
                "trait" => {
                    let id = Uuid::parse_str(&key).map_err(|_| DatabaseError::QueryFailed)?;
                    snapshot.trait_ids_by_name.insert(entry.base.clone(), id);
                    snapshot.traits.insert(id, entry);
                }
                "question" => {
                    snapshot.questions.insert(
                        Uuid::parse_str(&key).map_err(|_| DatabaseError::QueryFailed)?,
                        entry,
                    );
                }
                "plan" => {
                    snapshot.plans.insert(key, entry);
                }
                "feature" => {
                    snapshot.features.insert(key, entry);
                }
                _ => return Err(DatabaseError::QueryFailed),
            }
        }
        Ok(snapshot)
    }
}

impl super::store::LocalizationStore for PgLocalizationRepository {
    fn language(&self, user_id: Uuid) -> super::store::LocalizationFuture<'_, Language> {
        Box::pin(self.read_language(user_id))
    }
    fn set_language(
        &self,
        user_id: Uuid,
        language: Language,
    ) -> super::store::LocalizationFuture<'_, Language> {
        Box::pin(self.write_language(user_id, language))
    }
    fn catalog(&self) -> super::store::LocalizationFuture<'_, CatalogSnapshot> {
        Box::pin(self.read_catalog())
    }
}

/// Apply a bounded patch in the same transaction as the canonical entity write.
/// French remains the canonical column, avoiding conflicting sources of truth.
pub(crate) async fn save_translations(
    connection: &mut sqlx::PgConnection,
    question: bool,
    id: Uuid,
    patch: &super::catalog::TranslationPatch,
) -> Result<(), crate::infra::postgres::DatabaseError> {
    use crate::infra::postgres::map_sqlx_error;
    for (language, value) in patch {
        if *language == Language::Fr {
            continue;
        }
        let (delete, upsert) = if question {
            (
                "DELETE FROM profile_question_translation WHERE question_id=$1 AND language=$2",
                "INSERT INTO profile_question_translation(question_id,language,prompt) VALUES($1,$2,$3) ON CONFLICT(question_id,language) DO UPDATE SET prompt=EXCLUDED.prompt",
            )
        } else {
            (
                "DELETE FROM trait_translation WHERE trait_id=$1 AND language=$2",
                "INSERT INTO trait_translation(trait_id,language,name) VALUES($1,$2,$3) ON CONFLICT(trait_id,language) DO UPDATE SET name=EXCLUDED.name",
            )
        };
        match value {
            Some(value) => {
                sqlx::query(upsert)
                    .bind(id)
                    .bind(language.as_str())
                    .bind(value)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
            }
            None => {
                sqlx::query(delete)
                    .bind(id)
                    .bind(language.as_str())
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
            }
        }
    }
    Ok(())
}
