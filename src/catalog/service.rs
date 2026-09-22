use std::collections::HashSet;
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization as _;
use uuid::Uuid;

use super::domain::{
    AdminProfileQuestion, PlanFeature, PreparedProfileAnswer, ProfileAnswer, ProfileAnswerInput,
    ProfileQuestion, ProfileQuestionInput, ProfileQuestionPatch, ReplaceAnswersOutcome,
    SubscriptionPlan, Trait,
};
use super::pg::CatalogStore;
use crate::infra::postgres::{ConstraintKind, DatabaseError};
use crate::moderation::text::TextModerator;
use crate::shared::text::{javascript_trim, utf8_len};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogError {
    InvalidTrait,
    TraitConflict,
    TraitNotFound,
    InvalidAnswers,
    ProfileNotFound,
    AnswerQuestionNotFound,
    QuestionNotFound,
    InvalidQuestionPayload,
    MissingQuestionFields,
    QuestionConflict,
    Database(DatabaseError),
}

impl From<DatabaseError> for CatalogError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

#[derive(Clone)]
pub struct CatalogService {
    store: Arc<dyn CatalogStore>,
    moderator: TextModerator,
}

impl CatalogService {
    pub fn new(store: Arc<dyn CatalogStore>) -> Self {
        Self {
            store,
            moderator: TextModerator,
        }
    }

    pub async fn plans(&self) -> Result<Vec<SubscriptionPlan>, CatalogError> {
        let rows = self.store.list_plan_rows().await?;
        let mut plans = Vec::<SubscriptionPlan>::new();
        for row in rows {
            let needs_plan = plans.last().is_none_or(|plan| plan.code != row.code);
            if needs_plan {
                plans.push(SubscriptionPlan {
                    code: row.code.clone(),
                    display_name: row.display_name,
                    monthly_price_cents: row.monthly_price_cents,
                    annual_price_cents: row.annual_price_cents,
                    currency: row.currency,
                    trial_days: row.trial_days,
                    weekly_continuation_limit: row.weekly_continuation_limit,
                    features: Vec::new(),
                });
            }
            if let Some(code) = row.feature_code {
                let plan = plans
                    .last_mut()
                    .ok_or(CatalogError::Database(DatabaseError::QueryFailed))?;
                plan.features.push(PlanFeature {
                    code,
                    display_name: row.feature_name,
                    description: row.feature_description,
                    feature_value: row.feature_value,
                });
            }
        }
        Ok(plans)
    }

    pub async fn traits(&self) -> Result<Vec<Trait>, CatalogError> {
        self.store.list_traits().await.map_err(Into::into)
    }

    pub async fn traits_for_user(&self, user_id: Uuid) -> Result<Vec<Trait>, CatalogError> {
        self.store
            .list_traits_for_user(user_id)
            .await
            .map_err(Into::into)
    }

    pub async fn create_trait(&self, name: String) -> Result<Trait, CatalogError> {
        let trait_value = Trait {
            id: Uuid::new_v4(),
            name: normalize_trait(&name)?,
        };
        match self.store.create_trait(trait_value.clone()).await {
            Ok(()) => Ok(trait_value),
            Err(DatabaseError::Constraint(ConstraintKind::Unique)) => {
                Err(CatalogError::TraitConflict)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn update_trait(&self, id: Uuid, name: String) -> Result<(), CatalogError> {
        let name = normalize_trait(&name)?;
        match self.store.update_trait(id, name).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(CatalogError::TraitNotFound),
            Err(DatabaseError::Constraint(ConstraintKind::Unique)) => {
                Err(CatalogError::TraitConflict)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn delete_trait(&self, id: Uuid) -> Result<(), CatalogError> {
        if self.store.delete_trait(id).await? {
            Ok(())
        } else {
            Err(CatalogError::TraitNotFound)
        }
    }

    pub async fn add_trait_to_user(
        &self,
        user_id: Uuid,
        trait_id: Uuid,
    ) -> Result<(), CatalogError> {
        if !self.store.trait_exists(trait_id).await? {
            return Err(CatalogError::TraitNotFound);
        }
        self.store.add_trait_to_user(user_id, trait_id).await?;
        Ok(())
    }

    pub async fn remove_trait_from_user(
        &self,
        user_id: Uuid,
        trait_id: Uuid,
    ) -> Result<(), CatalogError> {
        self.store.remove_trait_from_user(user_id, trait_id).await?;
        Ok(())
    }

    pub async fn questions(&self) -> Result<Vec<ProfileQuestion>, CatalogError> {
        self.store.list_questions().await.map_err(Into::into)
    }

    pub async fn questions_for_admin(&self) -> Result<Vec<AdminProfileQuestion>, CatalogError> {
        self.store
            .list_questions_for_admin()
            .await
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    pub async fn answers_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProfileAnswer>, CatalogError> {
        self.store
            .list_answers_for_user(user_id)
            .await
            .map_err(Into::into)
    }

    pub async fn replace_answers_for_user(
        &self,
        user_id: Uuid,
        input: Vec<ProfileAnswerInput>,
    ) -> Result<Vec<ProfileAnswer>, CatalogError> {
        let answers = prepare_answers(self.moderator, input)?;
        match self
            .store
            .replace_answers_for_user(user_id, answers)
            .await?
        {
            ReplaceAnswersOutcome::Updated => self.answers_for_user(user_id).await,
            ReplaceAnswersOutcome::ProfileNotFound => Err(CatalogError::ProfileNotFound),
            ReplaceAnswersOutcome::QuestionNotFound => Err(CatalogError::AnswerQuestionNotFound),
        }
    }

    pub async fn create_question(
        &self,
        input: ProfileQuestionInput,
    ) -> Result<AdminProfileQuestion, CatalogError> {
        let input = normalize_question(input)?;
        let id = Uuid::new_v4();
        let code = format!("custom_{}", id.simple());
        match self.store.create_question(id, code, input).await {
            Ok(row) => Ok(row.into()),
            Err(DatabaseError::Constraint(ConstraintKind::Unique)) => {
                Err(CatalogError::QuestionConflict)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn update_question(
        &self,
        id: Uuid,
        mut patch: ProfileQuestionPatch,
    ) -> Result<AdminProfileQuestion, CatalogError> {
        if !patch.supplied {
            return Err(CatalogError::MissingQuestionFields);
        }
        if let Some(prompt) = patch.prompt {
            patch.prompt = Some(normalize_question_prompt(&prompt)?);
        }
        match self.store.update_question(id, patch).await {
            Ok(Some(row)) => Ok(row.into()),
            Ok(None) => Err(CatalogError::QuestionNotFound),
            Err(DatabaseError::Constraint(ConstraintKind::Unique)) => {
                Err(CatalogError::QuestionConflict)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn delete_question(&self, id: Uuid) -> Result<(), CatalogError> {
        if self.store.delete_question(id).await? {
            Ok(())
        } else {
            Err(CatalogError::QuestionNotFound)
        }
    }
}

fn normalize_trait(name: &str) -> Result<String, CatalogError> {
    let normalized = javascript_trim(name);
    if normalized.is_empty() || utf8_len(normalized) > 100 {
        return Err(CatalogError::InvalidTrait);
    }
    Ok(normalized.to_owned())
}

fn normalize_question(
    mut input: ProfileQuestionInput,
) -> Result<ProfileQuestionInput, CatalogError> {
    input.prompt = normalize_question_prompt(&input.prompt)?;
    Ok(input)
}

fn normalize_question_prompt(value: &str) -> Result<String, CatalogError> {
    let normalized = value.nfkc().collect::<String>();
    let normalized = javascript_trim(&normalized).to_owned();
    if !(3..=200).contains(&normalized.chars().count())
        || utf8_len(&normalized) > 500
        || has_control_character(&normalized)
    {
        return Err(CatalogError::InvalidQuestionPayload);
    }
    Ok(normalized)
}

fn normalize_answer(value: &str) -> Result<String, CatalogError> {
    let normalized = value.nfkc().collect::<String>();
    let normalized = javascript_trim(&normalized).to_owned();
    if !(10..=300).contains(&normalized.chars().count())
        || utf8_len(&normalized) > 1_000
        || has_control_character(&normalized)
    {
        return Err(CatalogError::InvalidAnswers);
    }
    Ok(normalized)
}

fn prepare_answers(
    moderator: TextModerator,
    input: Vec<ProfileAnswerInput>,
) -> Result<Vec<PreparedProfileAnswer>, CatalogError> {
    let question_ids = input
        .iter()
        .map(|answer| answer.question_id)
        .collect::<HashSet<_>>();
    if input.len() > 3 || question_ids.len() != input.len() {
        return Err(CatalogError::InvalidAnswers);
    }
    input
        .into_iter()
        .map(|answer| {
            let normalized = normalize_answer(&answer.answer)?;
            Ok(PreparedProfileAnswer {
                question_id: answer.question_id,
                moderation: moderator.analyze(&normalized),
                answer: normalized,
            })
        })
        .collect()
}

fn has_control_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| character <= '\u{001f}' || ('\u{007f}'..='\u{009f}').contains(&character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_nfkc_and_javascript_whitespace() {
        assert_eq!(
            normalize_question_prompt("\u{feff}Une question？\u{3000}"),
            Ok("Une question?".to_owned())
        );
        assert_eq!(
            normalize_answer("\u{00a0}Une réponse complète！\u{3000}"),
            Ok("Une réponse complète!".to_owned())
        );
    }

    #[test]
    fn enforces_character_byte_and_control_limits() {
        assert_eq!(
            normalize_answer("tropcourt"),
            Err(CatalogError::InvalidAnswers)
        );
        assert_eq!(normalize_answer(&"é".repeat(300)), Ok("é".repeat(300)));
        assert_eq!(
            normalize_answer(&"🦀".repeat(251)),
            Err(CatalogError::InvalidAnswers)
        );
        assert_eq!(
            normalize_answer("réponse avec\nretour"),
            Err(CatalogError::InvalidAnswers)
        );
    }

    #[test]
    fn trims_traits_by_javascript_rules_and_counts_utf8_bytes() {
        assert_eq!(
            normalize_trait("\u{feff} Curieux \u{3000}"),
            Ok("Curieux".to_owned())
        );
        assert_eq!(normalize_trait(""), Err(CatalogError::InvalidTrait));
        assert_eq!(
            normalize_trait(&"é".repeat(51)),
            Err(CatalogError::InvalidTrait)
        );
    }

    #[test]
    fn rejects_duplicate_questions_and_preserves_input_order() {
        let first = Uuid::new_v4();
        let duplicate = vec![
            ProfileAnswerInput {
                question_id: first,
                answer: "Première réponse valide".to_owned(),
            },
            ProfileAnswerInput {
                question_id: first,
                answer: "Deuxième réponse valide".to_owned(),
            },
        ];
        assert_eq!(
            prepare_answers(TextModerator, duplicate),
            Err(CatalogError::InvalidAnswers)
        );

        let second = Uuid::new_v4();
        let prepared = prepare_answers(
            TextModerator,
            vec![
                ProfileAnswerInput {
                    question_id: first,
                    answer: "  Première réponse valide  ".to_owned(),
                },
                ProfileAnswerInput {
                    question_id: second,
                    answer: "Deuxième réponse valide".to_owned(),
                },
            ],
        )
        .unwrap_or_else(|error| panic!("valid answers: {error:?}"));
        assert_eq!(prepared[0].question_id, first);
        assert_eq!(prepared[0].answer, "Première réponse valide");
        assert_eq!(prepared[1].question_id, second);
    }
}
