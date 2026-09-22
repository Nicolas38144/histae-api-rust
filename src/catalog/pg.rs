use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use super::domain::{
    AdminProfileQuestionRow, PlanRow, PreparedProfileAnswer, ProfileAnswer, ProfileQuestion,
    ProfileQuestionCategory, ProfileQuestionInput, ProfileQuestionPatch, ReplaceAnswersOutcome,
    Trait,
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::profiles::domain::{ModerationReason, ModerationStatus};

pub type CatalogStoreFuture<'store, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'store>>;

pub trait CatalogStore: Send + Sync {
    fn list_plan_rows(&self) -> CatalogStoreFuture<'_, Vec<PlanRow>>;
    fn list_traits(&self) -> CatalogStoreFuture<'_, Vec<Trait>>;
    fn list_traits_for_user(&self, user_id: Uuid) -> CatalogStoreFuture<'_, Vec<Trait>>;
    fn create_trait(&self, trait_value: Trait) -> CatalogStoreFuture<'_, ()>;
    fn update_trait(&self, id: Uuid, name: String) -> CatalogStoreFuture<'_, bool>;
    fn delete_trait(&self, id: Uuid) -> CatalogStoreFuture<'_, bool>;
    fn trait_exists(&self, id: Uuid) -> CatalogStoreFuture<'_, bool>;
    fn add_trait_to_user(&self, user_id: Uuid, trait_id: Uuid) -> CatalogStoreFuture<'_, ()>;
    fn remove_trait_from_user(&self, user_id: Uuid, trait_id: Uuid) -> CatalogStoreFuture<'_, ()>;
    fn list_questions(&self) -> CatalogStoreFuture<'_, Vec<ProfileQuestion>>;
    fn list_questions_for_admin(&self) -> CatalogStoreFuture<'_, Vec<AdminProfileQuestionRow>>;
    fn list_answers_for_user(&self, user_id: Uuid) -> CatalogStoreFuture<'_, Vec<ProfileAnswer>>;
    fn replace_answers_for_user(
        &self,
        user_id: Uuid,
        answers: Vec<PreparedProfileAnswer>,
    ) -> CatalogStoreFuture<'_, ReplaceAnswersOutcome>;
    fn create_question(
        &self,
        id: Uuid,
        code: String,
        input: ProfileQuestionInput,
    ) -> CatalogStoreFuture<'_, AdminProfileQuestionRow>;
    fn update_question(
        &self,
        id: Uuid,
        patch: ProfileQuestionPatch,
    ) -> CatalogStoreFuture<'_, Option<AdminProfileQuestionRow>>;
    fn delete_question(&self, id: Uuid) -> CatalogStoreFuture<'_, bool>;
}

#[derive(Clone)]
pub struct PgCatalogRepository {
    database: Database,
}

impl PgCatalogRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl CatalogStore for PgCatalogRepository {
    fn list_plan_rows(&self) -> CatalogStoreFuture<'_, Vec<PlanRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                r#"
                SELECT plan.code, plan.display_name, plan.monthly_price_cents,
                    plan.annual_price_cents, plan.currency,
                    plan.weekly_continuation_limit, plan.trial_days,
                    feature.feature_code, feature.display_name AS feature_name,
                    feature.description AS feature_description, feature.feature_value
                FROM subscription_plan AS plan
                LEFT JOIN subscription_plan_feature AS feature ON feature.plan_code = plan.code
                WHERE plan.is_active = true
                ORDER BY plan.monthly_price_cents, plan.code,
                    feature.sort_order, feature.feature_code
                "#,
            )
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(plan_row)
            .collect()
        })
    }

    fn list_traits(&self) -> CatalogStoreFuture<'_, Vec<Trait>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("SELECT id, name FROM trait ORDER BY name, id")
                .fetch_all(&mut *connection)
                .await
                .map_err(map_sqlx_error)?
                .into_iter()
                .map(trait_row)
                .collect()
        })
    }

    fn list_traits_for_user(&self, user_id: Uuid) -> CatalogStoreFuture<'_, Vec<Trait>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                r#"
                SELECT trait.id, trait.name
                FROM user_trait JOIN trait ON trait.id = user_trait.trait_id
                WHERE user_trait.user_id = $1
                ORDER BY trait.name, trait.id
                "#,
            )
            .bind(user_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(trait_row)
            .collect()
        })
    }

    fn create_trait(&self, trait_value: Trait) -> CatalogStoreFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("INSERT INTO trait (id, name) VALUES ($1, $2)")
                .bind(trait_value.id)
                .bind(trait_value.name)
                .execute(&mut *connection)
                .await
                .map(|_| ())
                .map_err(map_sqlx_error)
        })
    }

    fn update_trait(&self, id: Uuid, name: String) -> CatalogStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("UPDATE trait SET name = $2 WHERE id = $1")
                .bind(id)
                .bind(name)
                .execute(&mut *connection)
                .await
                .map(|result| result.rows_affected() == 1)
                .map_err(map_sqlx_error)
        })
    }

    fn delete_trait(&self, id: Uuid) -> CatalogStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("DELETE FROM trait WHERE id = $1")
                .bind(id)
                .execute(&mut *connection)
                .await
                .map(|result| result.rows_affected() == 1)
                .map_err(map_sqlx_error)
        })
    }

    fn trait_exists(&self, id: Uuid) -> CatalogStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM trait WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *connection)
                .await
                .map_err(map_sqlx_error)
        })
    }

    fn add_trait_to_user(&self, user_id: Uuid, trait_id: Uuid) -> CatalogStoreFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                r#"
                INSERT INTO user_trait (user_id, trait_id)
                VALUES ($1, $2)
                ON CONFLICT (user_id, trait_id) DO NOTHING
                "#,
            )
            .bind(user_id)
            .bind(trait_id)
            .execute(&mut *connection)
            .await
            .map(|_| ())
            .map_err(map_sqlx_error)
        })
    }

    fn remove_trait_from_user(&self, user_id: Uuid, trait_id: Uuid) -> CatalogStoreFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("DELETE FROM user_trait WHERE user_id = $1 AND trait_id = $2")
                .bind(user_id)
                .bind(trait_id)
                .execute(&mut *connection)
                .await
                .map(|_| ())
                .map_err(map_sqlx_error)
        })
    }

    fn list_questions(&self) -> CatalogStoreFuture<'_, Vec<ProfileQuestion>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                "SELECT id, code, prompt, category, display_order FROM profile_question ORDER BY display_order, id",
            )
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(question_row)
            .collect()
        })
    }

    fn list_questions_for_admin(&self) -> CatalogStoreFuture<'_, Vec<AdminProfileQuestionRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                r#"
                SELECT question.id, question.code, question.prompt, question.category,
                    question.display_order, question.created_at, question.updated_at,
                    count(answer.id)::integer AS answer_count
                FROM profile_question AS question
                LEFT JOIN user_profile_answer AS answer ON answer.question_id = question.id
                GROUP BY question.id
                ORDER BY question.display_order, question.id
                "#,
            )
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(admin_question_row)
            .collect()
        })
    }

    fn list_answers_for_user(&self, user_id: Uuid) -> CatalogStoreFuture<'_, Vec<ProfileAnswer>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query(
                r#"
                SELECT answer.question_id, question.code, question.prompt AS question,
                    answer.answer, answer.position::integer AS position,
                    moderation.status AS moderation_status,
                    moderation.reason_codes AS moderation_reasons
                FROM user_profile_answer AS answer
                JOIN profile_question AS question ON question.id = answer.question_id
                JOIN content_moderation_case AS moderation
                    ON moderation.profile_answer_id = answer.id
                WHERE answer.user_id = $1
                ORDER BY answer.position
                "#,
            )
            .bind(user_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?
            .into_iter()
            .map(answer_row)
            .collect()
        })
    }

    fn replace_answers_for_user(
        &self,
        user_id: Uuid,
        answers: Vec<PreparedProfileAnswer>,
    ) -> CatalogStoreFuture<'_, ReplaceAnswersOutcome> {
        Box::pin(async move {
            self.database
                .transaction(move |connection| {
                    Box::pin(async move {
                        let profile_exists = sqlx::query_scalar::<_, Uuid>(
                            r#"
                            SELECT profile.user_id
                            FROM user_profile AS profile
                            JOIN user_account AS account ON account.user_id = profile.user_id
                            WHERE profile.user_id = $1 AND account.deleted_at IS NULL
                            FOR UPDATE OF profile
                            "#,
                        )
                        .bind(user_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .is_some();
                        if !profile_exists {
                            return Ok(ReplaceAnswersOutcome::ProfileNotFound);
                        }

                        let question_ids = answers
                            .iter()
                            .map(|answer| answer.question_id)
                            .collect::<Vec<_>>();
                        if !question_ids.is_empty() {
                            let count = sqlx::query_scalar::<_, Uuid>(
                                "SELECT id FROM profile_question WHERE id = ANY($1::uuid[]) FOR SHARE",
                            )
                            .bind(&question_ids)
                            .fetch_all(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?
                            .len();
                            if count != question_ids.len() {
                                return Ok(ReplaceAnswersOutcome::QuestionNotFound);
                            }
                        }

                        sqlx::query("DELETE FROM user_profile_answer WHERE user_id = $1")
                            .bind(user_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        for (index, answer) in answers.into_iter().enumerate() {
                            insert_answer(connection, user_id, index, answer).await?;
                        }
                        Ok(ReplaceAnswersOutcome::Updated)
                    })
                })
                .await
        })
    }

    fn create_question(
        &self,
        id: Uuid,
        code: String,
        input: ProfileQuestionInput,
    ) -> CatalogStoreFuture<'_, AdminProfileQuestionRow> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                r#"
                INSERT INTO profile_question (id, code, prompt, category, display_order)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id, code, prompt, category, display_order, created_at, updated_at,
                    0::integer AS answer_count
                "#,
            )
            .bind(id)
            .bind(code)
            .bind(input.prompt)
            .bind(input.category.as_str())
            .bind(input.display_order)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            admin_question_row(row)
        })
    }

    fn update_question(
        &self,
        id: Uuid,
        patch: ProfileQuestionPatch,
    ) -> CatalogStoreFuture<'_, Option<AdminProfileQuestionRow>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let category = patch.category.map(ProfileQuestionCategory::as_str);
            let row = sqlx::query(
                r#"
                UPDATE profile_question AS question
                SET prompt = COALESCE($2, prompt), category = COALESCE($3, category),
                    display_order = COALESCE($4, display_order), updated_at = clock_timestamp()
                WHERE id = $1
                RETURNING id, code, prompt, category, display_order, created_at, updated_at,
                    (SELECT count(*)::integer FROM user_profile_answer
                     WHERE question_id = question.id) AS answer_count
                "#,
            )
            .bind(id)
            .bind(patch.prompt)
            .bind(category)
            .bind(patch.display_order)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(admin_question_row).transpose()
        })
    }

    fn delete_question(&self, id: Uuid) -> CatalogStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query("DELETE FROM profile_question WHERE id = $1")
                .bind(id)
                .execute(&mut *connection)
                .await
                .map(|result| result.rows_affected() == 1)
                .map_err(map_sqlx_error)
        })
    }
}

async fn insert_answer(
    connection: &mut PgConnection,
    user_id: Uuid,
    index: usize,
    answer: PreparedProfileAnswer,
) -> Result<(), DatabaseError> {
    let position = i16::try_from(index + 1).map_err(|_| DatabaseError::QueryFailed)?;
    let answer_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO user_profile_answer (user_id, question_id, answer, position)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
    )
    .bind(user_id)
    .bind(answer.question_id)
    .bind(answer.answer)
    .bind(position)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let reasons = answer
        .moderation
        .reasons
        .into_iter()
        .map(ModerationReason::as_str)
        .collect::<Vec<_>>();
    sqlx::query(
        r#"
        INSERT INTO content_moderation_case (
            user_id, content_type, profile_answer_id, status,
            reason_codes, policy_version
        ) VALUES ($1, 'profile_answer', $2, $3, $4, $5)
        "#,
    )
    .bind(user_id)
    .bind(answer_id)
    .bind(answer.moderation.status.as_str())
    .bind(reasons)
    .bind(answer.moderation.policy_version)
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(map_sqlx_error)
}

fn plan_row(row: sqlx::postgres::PgRow) -> Result<PlanRow, DatabaseError> {
    Ok(PlanRow {
        code: get(&row, "code")?,
        display_name: get(&row, "display_name")?,
        monthly_price_cents: get(&row, "monthly_price_cents")?,
        annual_price_cents: get(&row, "annual_price_cents")?,
        currency: get::<String>(&row, "currency")?.trim_end().to_owned(),
        weekly_continuation_limit: get::<Option<i16>>(&row, "weekly_continuation_limit")?
            .map(i32::from),
        trial_days: i32::from(get::<i16>(&row, "trial_days")?),
        feature_code: get(&row, "feature_code")?,
        feature_name: get(&row, "feature_name")?,
        feature_description: get(&row, "feature_description")?,
        feature_value: get(&row, "feature_value")?,
    })
}

fn trait_row(row: sqlx::postgres::PgRow) -> Result<Trait, DatabaseError> {
    Ok(Trait {
        id: get(&row, "id")?,
        name: get(&row, "name")?,
    })
}

fn question_row(row: sqlx::postgres::PgRow) -> Result<ProfileQuestion, DatabaseError> {
    let category = get::<String>(&row, "category")?;
    Ok(ProfileQuestion {
        id: get(&row, "id")?,
        code: get(&row, "code")?,
        prompt: get(&row, "prompt")?,
        category: ProfileQuestionCategory::parse(&category).ok_or(DatabaseError::QueryFailed)?,
        display_order: get(&row, "display_order")?,
    })
}

fn admin_question_row(
    row: sqlx::postgres::PgRow,
) -> Result<AdminProfileQuestionRow, DatabaseError> {
    let category = get::<String>(&row, "category")?;
    Ok(AdminProfileQuestionRow {
        question: ProfileQuestion {
            id: get(&row, "id")?,
            code: get(&row, "code")?,
            prompt: get(&row, "prompt")?,
            category: ProfileQuestionCategory::parse(&category)
                .ok_or(DatabaseError::QueryFailed)?,
            display_order: get(&row, "display_order")?,
        },
        answer_count: get(&row, "answer_count")?,
        created_at: get::<DateTime<Utc>>(&row, "created_at")?,
        updated_at: get::<DateTime<Utc>>(&row, "updated_at")?,
    })
}

fn answer_row(row: sqlx::postgres::PgRow) -> Result<ProfileAnswer, DatabaseError> {
    let status = get::<String>(&row, "moderation_status")?;
    let reasons = get::<Vec<String>>(&row, "moderation_reasons")?
        .into_iter()
        .map(|reason| ModerationReason::parse(&reason).ok_or(DatabaseError::QueryFailed))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ProfileAnswer {
        question_id: get(&row, "question_id")?,
        code: get(&row, "code")?,
        question: get(&row, "question")?,
        answer: get(&row, "answer")?,
        position: get(&row, "position")?,
        moderation_status: ModerationStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?,
        moderation_reasons: reasons,
    })
}

fn get<T>(row: &sqlx::postgres::PgRow, column: &str) -> Result<T, DatabaseError>
where
    for<'value> T: sqlx::Decode<'value, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(|_| DatabaseError::QueryFailed)
}
