#[cfg(feature = "webauthn-probe")]
use crate::localization::catalog::TranslationPatch;
use crate::localization::http::Locale;
#[cfg(feature = "webauthn-probe")]
use std::fmt;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
#[cfg(feature = "webauthn-probe")]
use serde::Deserializer;
#[cfg(feature = "webauthn-probe")]
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "webauthn-probe")]
use super::domain::{
    AdminProfileQuestion, ProfileQuestionCategory, ProfileQuestionInput, ProfileQuestionPatch,
};
use super::domain::{PlanCatalog, ProfileAnswer, ProfileAnswerInput, ProfileQuestion, Trait};
use super::service::{CatalogError, CatalogService};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath};
use crate::http::router::HttpState;
#[cfg(feature = "webauthn-probe")]
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};

#[derive(Clone)]
pub struct CatalogHttpState {
    service: CatalogService,
}

impl CatalogHttpState {
    pub fn new(service: CatalogService) -> Self {
        Self { service }
    }
}

#[cfg(feature = "webauthn-probe")]
pub fn routes(
    state: CatalogHttpState,
    mobile_auth: MobileAuthState,
    admin_auth: AdminAuthHttpState,
) -> Router<HttpState> {
    public_mobile_routes(state.clone(), mobile_auth).merge(admin_routes(state, admin_auth))
}

#[cfg(not(feature = "webauthn-probe"))]
pub fn routes(state: CatalogHttpState, mobile_auth: MobileAuthState) -> Router<HttpState> {
    public_mobile_routes(state, mobile_auth)
}

fn public_mobile_routes(
    state: CatalogHttpState,
    mobile_auth: MobileAuthState,
) -> Router<HttpState> {
    Router::new()
        .route("/api/plans", get(list_plans))
        .route("/api/traits", get(list_traits))
        .route(
            "/api/users/me/traits",
            get(list_my_traits).post(add_my_trait),
        )
        .route("/api/users/me/traits/{traitId}", delete(remove_my_trait))
        .route("/api/profile-questions", get(list_questions))
        .route(
            "/api/users/me/profile-answers",
            get(list_my_answers).put(replace_my_answers),
        )
        .layer(Extension(state))
        .layer(Extension(mobile_auth))
}

#[cfg(feature = "webauthn-probe")]
fn admin_routes(state: CatalogHttpState, admin_auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/admin/traits",
            get(list_admin_traits).post(create_trait),
        )
        .route(
            "/api/admin/traits/{id}",
            axum::routing::patch(update_trait).delete(delete_trait),
        )
        .route(
            "/api/admin/profile-questions",
            get(list_admin_questions).post(create_question),
        )
        .route(
            "/api/admin/profile-questions/{id}",
            axum::routing::patch(update_question).delete(delete_question),
        )
        .layer(Extension(state))
        .layer(Extension(admin_auth))
}

async fn list_plans(
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<PlanCatalog>, ApiError> {
    localized_plans(&state.service, locale).await.map(Json)
}

pub(crate) async fn localized_plans(
    service: &CatalogService,
    locale: Locale,
) -> Result<PlanCatalog, ApiError> {
    let locale = locale.anonymous();
    let catalog = locale.catalog(false).await?;
    let mut plans = service.plans().await.map_err(catalog_error)?;
    for plan in &mut plans {
        if let Some(entry) = catalog.plans.get(&plan.code) {
            plan.display_name = entry.name(locale.language).to_owned();
        }
        for feature in &mut plan.features {
            if let Some(entry) = catalog
                .features
                .get(&format!("{}:{}", plan.code, feature.code))
            {
                feature.display_name = Some(entry.name(locale.language).to_owned());
                if let Some(description) = entry
                    .descriptions
                    .get(&locale.language)
                    .or_else(|| entry.descriptions.get(&crate::localization::Language::Fr))
                {
                    feature.description = Some(description.clone());
                }
            }
        }
    }
    let mut response = PlanCatalog::new(plans);
    for offer in &mut response.offers {
        if let Some(period) = offer.billing_period {
            if let Some(plan) = response
                .plans
                .iter()
                .find(|plan| plan.code == offer.plan_code)
            {
                offer.display_name = crate::localization::copy::offer(
                    locale.language,
                    &plan.display_name,
                    period == crate::billing::domain::BillingPeriod::Annual,
                );
            }
        }
    }
    Ok(response)
}

#[derive(Serialize)]
struct TraitsResponse {
    traits: Vec<Trait>,
}

async fn list_traits(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<TraitsResponse>, ApiError> {
    let locale = locale.for_account(&identity.account);
    let mut traits = state.service.traits().await.map_err(catalog_error)?;
    let catalog = locale.catalog(false).await?;
    for item in &mut traits {
        catalog.trait_name(item.id, &mut item.name, locale.language);
    }
    Ok(Json(TraitsResponse { traits }))
}

async fn list_my_traits(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<TraitsResponse>, ApiError> {
    let locale = locale.for_account(&identity.account);
    let mut traits = state
        .service
        .traits_for_user(identity.account.user_id)
        .await
        .map_err(catalog_error)?;
    let catalog = locale.catalog(false).await?;
    for item in &mut traits {
        catalog.trait_name(item.id, &mut item.name, locale.language);
    }
    Ok(Json(TraitsResponse { traits }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserTraitBody {
    #[serde(rename = "traitId")]
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    trait_id: Uuid,
}

impl ApiDto for UserTraitBody {
    const ERROR_CODE: &'static str = "invalid_user_trait_payload";
    const ERROR_MESSAGE: &'static str = "The user trait request body is invalid.";

    fn is_valid(&self) -> bool {
        valid_uuid_all(self.trait_id)
    }
}

async fn add_my_trait(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedJson(body): ValidatedJson<UserTraitBody>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .add_trait_to_user(identity.account.user_id, body.trait_id)
        .await
        .map_err(catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UserTraitPath {
    #[serde(rename = "traitId")]
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    trait_id: Uuid,
}

impl ApiDto for UserTraitPath {
    const ERROR_CODE: &'static str = "invalid_trait_id";
    const ERROR_MESSAGE: &'static str = "The trait ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        valid_uuid_all(self.trait_id)
    }
}

async fn remove_my_trait(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedPath(path): ValidatedPath<UserTraitPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .remove_trait_from_user(identity.account.user_id, path.trait_id)
        .await
        .map_err(catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct AdminTraitsResponse {
    traits: Vec<crate::localization::catalog::Translated<Trait>>,
}

#[cfg(feature = "webauthn-probe")]
async fn list_admin_traits(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<AdminTraitsResponse>, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    let traits = state.service.traits().await.map_err(catalog_error)?;
    let catalog = locale.catalog(true).await?;
    let traits = traits
        .into_iter()
        .map(|mut value| {
            let translations = catalog.trait_translations(value.id, &value.name);
            catalog.trait_name(value.id, &mut value.name, locale.language);
            crate::localization::catalog::Translated {
                value,
                translations,
            }
        })
        .collect();
    Ok(Json(AdminTraitsResponse { traits }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraitBody {
    name: String,
    #[serde(default)]
    translations: TranslationPatch,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for TraitBody {
    const ERROR_CODE: &'static str = "invalid_trait_payload";
    const ERROR_MESSAGE: &'static str = "The trait request body is invalid.";
}

#[cfg(feature = "webauthn-probe")]
async fn create_trait(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<TraitBody>,
) -> Result<
    (
        StatusCode,
        Json<crate::localization::catalog::Translated<Trait>>,
    ),
    ApiError,
> {
    let locale = locale.resolve(identity.user_id).await?;
    let translations =
        super::service::normalize_translations(body.translations.clone(), None, false)
            .map_err(catalog_error)?;
    let value = state
        .service
        .create_translated_trait(body.name, body.translations)
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    let mut names =
        std::collections::BTreeMap::from([(crate::localization::Language::Fr, value.name.clone())]);
    names.extend(
        translations
            .into_iter()
            .filter_map(|(language, text)| text.map(|text| (language, text.trim().to_owned()))),
    );
    Ok((
        StatusCode::CREATED,
        Json(crate::localization::catalog::Translated {
            value,
            translations: names,
        }),
    ))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
struct IdPath {
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    id: Uuid,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for IdPath {
    const ERROR_CODE: &'static str = "invalid_trait_id";
    const ERROR_MESSAGE: &'static str = "The trait ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        valid_uuid_all(self.id)
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

#[cfg(feature = "webauthn-probe")]
async fn update_trait(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedJson(body): ValidatedJson<TraitBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    state
        .service
        .update_translated_trait(path.id, body.name, body.translations)
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    Ok(Json(MessageResponse {
        message: "trait updated",
    }))
}

#[cfg(feature = "webauthn-probe")]
async fn delete_trait(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedPath(path): ValidatedPath<IdPath>,
) -> Result<StatusCode, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    state
        .service
        .delete_trait(path.id)
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct QuestionsResponse {
    questions: Vec<ProfileQuestion>,
}

async fn list_questions(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<QuestionsResponse>, ApiError> {
    let locale = locale.for_account(&identity.account);
    let mut questions = state.service.questions().await.map_err(catalog_error)?;
    let catalog = locale.catalog(false).await?;
    for item in &mut questions {
        catalog.question(item.id, &mut item.prompt, locale.language);
    }
    Ok(Json(QuestionsResponse { questions }))
}

#[derive(Serialize)]
struct AnswersResponse {
    answers: Vec<ProfileAnswer>,
}

async fn list_my_answers(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<AnswersResponse>, ApiError> {
    let locale = locale.for_account(&identity.account);
    let mut answers = state
        .service
        .answers_for_user(identity.account.user_id)
        .await
        .map_err(catalog_error)?;
    let catalog = locale.catalog(false).await?;
    for item in &mut answers {
        catalog.question(item.question_id, &mut item.question, locale.language);
    }
    Ok(Json(AnswersResponse { answers }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    question_id: Uuid,
    answer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceAnswersBody {
    answers: Vec<AnswerBody>,
}

impl ApiDto for ReplaceAnswersBody {
    const ERROR_CODE: &'static str = "invalid_profile_answers";
    const ERROR_MESSAGE: &'static str = "The profile answers request body is invalid.";

    fn is_valid(&self) -> bool {
        self.answers.len() <= 3
            && self.answers.iter().all(|answer| {
                valid_uuid_all(answer.question_id)
                    && (10..=300).contains(&answer.answer.chars().count())
            })
    }
}

async fn replace_my_answers(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<ReplaceAnswersBody>,
) -> Result<Json<AnswersResponse>, ApiError> {
    let locale = locale.for_account(&identity.account);
    // Fetch localization before mutation so an unavailable catalogue cannot hide a successful write.
    let catalog = locale.catalog(false).await?;
    let input = body
        .answers
        .into_iter()
        .map(|answer| ProfileAnswerInput {
            question_id: answer.question_id,
            answer: answer.answer,
        })
        .collect();
    let mut answers = state
        .service
        .replace_answers_for_user(identity.account.user_id, input)
        .await
        .map_err(catalog_error)?;
    for item in &mut answers {
        catalog.question(item.question_id, &mut item.question, locale.language);
    }
    Ok(Json(AnswersResponse { answers }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct AdminQuestionsResponse {
    questions: Vec<crate::localization::catalog::Translated<AdminProfileQuestion>>,
}

#[cfg(feature = "webauthn-probe")]
async fn list_admin_questions(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
) -> Result<Json<AdminQuestionsResponse>, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    let questions = state
        .service
        .questions_for_admin()
        .await
        .map_err(catalog_error)?;
    let catalog = locale.catalog(true).await?;
    let questions = questions
        .into_iter()
        .map(|mut value| {
            let translations = catalog.question_translations(value.id, &value.prompt);
            catalog.question(value.id, &mut value.prompt, locale.language);
            crate::localization::catalog::Translated {
                value,
                translations,
            }
        })
        .collect();
    Ok(Json(AdminQuestionsResponse { questions }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Clone, Copy, Debug)]
struct DisplayOrder(i32);

#[cfg(feature = "webauthn-probe")]
impl DisplayOrder {
    fn valid(self) -> bool {
        (0..=10_000).contains(&self.0)
    }
}

#[cfg(feature = "webauthn-probe")]
impl<'de> Deserialize<'de> for DisplayOrder {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DisplayOrderVisitor)
    }
}

#[cfg(feature = "webauthn-probe")]
struct DisplayOrderVisitor;

#[cfg(feature = "webauthn-probe")]
impl Visitor<'_> for DisplayOrderVisitor {
    type Value = DisplayOrder;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JavaScript-coercible integer")
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i32::try_from(value)
            .map(DisplayOrder)
            .map_err(|_| E::custom("display order is outside i32"))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i32::try_from(value)
            .map(DisplayOrder)
            .map_err(|_| E::custom("display order is outside i32"))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_finite()
            && value.fract() == 0.0
            && value >= i32::MIN as f64
            && value <= i32::MAX as f64
        {
            Ok(DisplayOrder(value as i32))
        } else {
            Err(E::custom("display order must be an integer"))
        }
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let number = if crate::shared::text::javascript_trim(value).is_empty() {
            0.0
        } else {
            crate::shared::validation::javascript_number(value)
                .ok_or(())
                .map_err(|_| E::custom("display order must be numeric"))?
        };
        self.visit_f64(number)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(DisplayOrder(i32::from(value)))
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Clone, Debug, Default)]
enum OptionalField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

#[cfg(feature = "webauthn-probe")]
impl<T> OptionalField<T> {
    fn is_supplied(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }
}

#[cfg(feature = "webauthn-probe")]
impl<'de, T> Deserialize<'de> for OptionalField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateQuestionBody {
    prompt: String,
    #[serde(default)]
    translations: TranslationPatch,
    category: ProfileQuestionCategory,
    #[serde(default)]
    display_order: OptionalField<DisplayOrder>,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for CreateQuestionBody {
    const ERROR_CODE: &'static str = "invalid_profile_question_payload";
    const ERROR_MESSAGE: &'static str = "The profile question request body is invalid.";

    fn is_valid(&self) -> bool {
        (3..=200).contains(&self.prompt.chars().count())
            && match self.display_order {
                OptionalField::Missing | OptionalField::Null => true,
                OptionalField::Value(value) => value.valid(),
            }
    }
}

#[cfg(feature = "webauthn-probe")]
async fn create_question(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedJson(body): ValidatedJson<CreateQuestionBody>,
) -> Result<
    (
        StatusCode,
        Json<crate::localization::catalog::Translated<AdminProfileQuestion>>,
    ),
    ApiError,
> {
    let locale = locale.resolve(identity.user_id).await?;
    let display_order = match body.display_order {
        OptionalField::Missing => 100,
        OptionalField::Value(value) => value.0,
        OptionalField::Null => return Err(ApiError::internal()),
    };
    let translations =
        super::service::normalize_translations(body.translations.clone(), None, true)
            .map_err(catalog_error)?;
    let value = state
        .service
        .create_translated_question(
            ProfileQuestionInput {
                prompt: body.prompt,
                category: body.category,
                display_order,
            },
            body.translations,
        )
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    let mut names = std::collections::BTreeMap::from([(
        crate::localization::Language::Fr,
        value.prompt.clone(),
    )]);
    names.extend(
        translations
            .into_iter()
            .filter_map(|(language, text)| text.map(|text| (language, text.trim().to_owned()))),
    );
    Ok((
        StatusCode::CREATED,
        Json(crate::localization::catalog::Translated {
            value,
            translations: names,
        }),
    ))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateQuestionBody {
    #[serde(default)]
    translations: TranslationPatch,
    #[serde(default)]
    prompt: OptionalField<String>,
    #[serde(default)]
    category: OptionalField<ProfileQuestionCategory>,
    #[serde(default)]
    display_order: OptionalField<DisplayOrder>,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for UpdateQuestionBody {
    const ERROR_CODE: &'static str = "invalid_profile_question_payload";
    const ERROR_MESSAGE: &'static str = "The profile question request body is invalid.";

    fn is_valid(&self) -> bool {
        let prompt_valid = match &self.prompt {
            OptionalField::Value(value) => (3..=200).contains(&value.chars().count()),
            OptionalField::Missing | OptionalField::Null => true,
        };
        let order_valid = match self.display_order {
            OptionalField::Value(value) => value.valid(),
            OptionalField::Missing | OptionalField::Null => true,
        };
        prompt_valid && order_valid
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
struct QuestionIdPath {
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    id: Uuid,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for QuestionIdPath {
    const ERROR_CODE: &'static str = "invalid_profile_question_id";
    const ERROR_MESSAGE: &'static str = "The profile question ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        valid_uuid_all(self.id)
    }
}

#[cfg(feature = "webauthn-probe")]
async fn update_question(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedPath(path): ValidatedPath<QuestionIdPath>,
    ValidatedJson(body): ValidatedJson<UpdateQuestionBody>,
) -> Result<Json<crate::localization::catalog::Translated<AdminProfileQuestion>>, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    let supplied = body.prompt.is_supplied()
        || body.category.is_supplied()
        || body.display_order.is_supplied()
        || !body.translations.is_empty();
    let value = state
        .service
        .update_translated_question(
            path.id,
            ProfileQuestionPatch {
                prompt: body.prompt.into_option(),
                category: body.category.into_option(),
                display_order: body.display_order.into_option().map(|value| value.0),
                supplied,
            },
            body.translations,
        )
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    Ok(Json(value))
}

#[cfg(feature = "webauthn-probe")]
async fn delete_question(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    locale: Locale,
    ValidatedPath(path): ValidatedPath<QuestionIdPath>,
) -> Result<StatusCode, ApiError> {
    let locale = locale.resolve(identity.user_id).await?;
    state
        .service
        .delete_question(path.id)
        .await
        .map_err(catalog_error)?;
    locale.invalidate().await;
    Ok(StatusCode::NO_CONTENT)
}

fn valid_uuid_all(value: Uuid) -> bool {
    crate::shared::validation::uuid_all(value)
}

fn catalog_error(error: CatalogError) -> ApiError {
    match error {
        CatalogError::InvalidTrait => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_trait_request",
            "The trait request is invalid.",
        ),
        CatalogError::TraitConflict => ApiError::new(
            StatusCode::CONFLICT,
            "trait_already_exists",
            "A trait with this name already exists.",
        ),
        CatalogError::TraitNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "trait_not_found",
            "The trait could not be found.",
        ),
        CatalogError::InvalidAnswers => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_profile_answers",
            "Profile answers must use distinct questions and contain between 10 and 300 characters.",
        ),
        CatalogError::ProfileNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "profile_not_found",
            "The account exists, but its profile has not been completed yet.",
        ),
        CatalogError::AnswerQuestionNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "profile_question_not_found",
            "At least one profile question could not be found.",
        ),
        CatalogError::QuestionNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "profile_question_not_found",
            "The profile question could not be found.",
        ),
        CatalogError::InvalidQuestionPayload => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_profile_question_payload",
            "The profile question request body is invalid.",
        ),
        CatalogError::MissingQuestionFields => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_profile_question_payload",
            "At least one profile question field must be provided.",
        ),
        CatalogError::QuestionConflict => ApiError::new(
            StatusCode::CONFLICT,
            "profile_question_already_exists",
            "A profile question with this prompt already exists.",
        ),
        CatalogError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_dtos_reject_compact_text_but_keep_validator_all_sentinels() {
        for id in [Uuid::new_v4(), Uuid::nil(), Uuid::max()] {
            let valid: UserTraitBody =
                serde_json::from_value(serde_json::json!({"traitId":id})).expect("canonical UUID");
            assert!(valid.is_valid());
            assert!(
                serde_json::from_value::<UserTraitBody>(
                    serde_json::json!({"traitId":id.simple().to_string()})
                )
                .is_err()
            );
        }
    }

    #[test]
    #[cfg(feature = "webauthn-probe")]
    fn accepts_class_transformer_number_coercions_for_display_order() {
        let body: CreateQuestionBody = serde_json::from_str(
            r#"{"prompt":"Une question","category":"conversation","display_order":" 42 "}"#,
        )
        .unwrap_or_else(|error| panic!("valid DTO: {error}"));
        assert!(body.is_valid());
        assert!(matches!(
            body.display_order,
            OptionalField::Value(DisplayOrder(42))
        ));

        let boolean: CreateQuestionBody = serde_json::from_str(
            r#"{"prompt":"Une question","category":"conversation","display_order":true}"#,
        )
        .unwrap_or_else(|error| panic!("valid DTO: {error}"));
        assert!(matches!(
            boolean.display_order,
            OptionalField::Value(DisplayOrder(1))
        ));
    }

    #[test]
    #[cfg(feature = "webauthn-probe")]
    fn distinguishes_missing_and_null_patch_fields() {
        let empty: UpdateQuestionBody =
            serde_json::from_str("{}").unwrap_or_else(|error| panic!("valid DTO: {error}"));
        assert!(!empty.prompt.is_supplied());
        let null: UpdateQuestionBody = serde_json::from_str(r#"{"prompt":null}"#)
            .unwrap_or_else(|error| panic!("valid DTO: {error}"));
        assert!(null.prompt.is_supplied());
        assert!(null.prompt.into_option().is_none());
    }

    #[test]
    fn keeps_the_mobile_trait_field_in_camel_case() {
        let id = Uuid::new_v4();
        let camel = format!(r#"{{"traitId":"{id}"}}"#);
        assert!(serde_json::from_str::<UserTraitBody>(&camel).is_ok());
        let snake = format!(r#"{{"trait_id":"{id}"}}"#);
        assert!(serde_json::from_str::<UserTraitBody>(&snake).is_err());
    }
}
