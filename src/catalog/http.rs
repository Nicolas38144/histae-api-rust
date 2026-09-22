#[cfg(feature = "webauthn-probe")]
use std::fmt;

use axum::extract::Extension;
use axum::http::StatusCode;
#[cfg(feature = "webauthn-probe")]
use axum::routing::post;
use axum::routing::{delete, get};
use axum::{Json, Router};
#[cfg(feature = "webauthn-probe")]
use serde::Deserializer;
#[cfg(feature = "webauthn-probe")]
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

#[cfg(feature = "webauthn-probe")]
use super::domain::{
    AdminProfileQuestion, ProfileQuestionCategory, ProfileQuestionInput, ProfileQuestionPatch,
};
use super::domain::{ProfileAnswer, ProfileAnswerInput, ProfileQuestion, SubscriptionPlan, Trait};
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
        .route("/api/admin/traits", post(create_trait))
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

#[derive(Serialize)]
struct PlansResponse {
    plans: Vec<SubscriptionPlan>,
}

async fn list_plans(
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<PlansResponse>, ApiError> {
    state
        .service
        .plans()
        .await
        .map(|plans| Json(PlansResponse { plans }))
        .map_err(catalog_error)
}

#[derive(Serialize)]
struct TraitsResponse {
    traits: Vec<Trait>,
}

async fn list_traits(
    OnboardedMobile(_identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<TraitsResponse>, ApiError> {
    state
        .service
        .traits()
        .await
        .map(|traits| Json(TraitsResponse { traits }))
        .map_err(catalog_error)
}

async fn list_my_traits(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<TraitsResponse>, ApiError> {
    state
        .service
        .traits_for_user(identity.account.user_id)
        .await
        .map(|traits| Json(TraitsResponse { traits }))
        .map_err(catalog_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserTraitBody {
    #[serde(rename = "traitId")]
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraitBody {
    name: String,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for TraitBody {
    const ERROR_CODE: &'static str = "invalid_trait_payload";
    const ERROR_MESSAGE: &'static str = "The trait request body is invalid.";
}

#[cfg(feature = "webauthn-probe")]
async fn create_trait(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedJson(body): ValidatedJson<TraitBody>,
) -> Result<(StatusCode, Json<Trait>), ApiError> {
    state
        .service
        .create_trait(body.name)
        .await
        .map(|trait_value| (StatusCode::CREATED, Json(trait_value)))
        .map_err(catalog_error)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
struct IdPath {
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
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedJson(body): ValidatedJson<TraitBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_trait(path.id, body.name)
        .await
        .map_err(catalog_error)?;
    Ok(Json(MessageResponse {
        message: "trait updated",
    }))
}

#[cfg(feature = "webauthn-probe")]
async fn delete_trait(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .delete_trait(path.id)
        .await
        .map_err(catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct QuestionsResponse {
    questions: Vec<ProfileQuestion>,
}

async fn list_questions(
    OnboardedMobile(_identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<QuestionsResponse>, ApiError> {
    state
        .service
        .questions()
        .await
        .map(|questions| Json(QuestionsResponse { questions }))
        .map_err(catalog_error)
}

#[derive(Serialize)]
struct AnswersResponse {
    answers: Vec<ProfileAnswer>,
}

async fn list_my_answers(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<AnswersResponse>, ApiError> {
    state
        .service
        .answers_for_user(identity.account.user_id)
        .await
        .map(|answers| Json(AnswersResponse { answers }))
        .map_err(catalog_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
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
    ValidatedJson(body): ValidatedJson<ReplaceAnswersBody>,
) -> Result<Json<AnswersResponse>, ApiError> {
    let input = body
        .answers
        .into_iter()
        .map(|answer| ProfileAnswerInput {
            question_id: answer.question_id,
            answer: answer.answer,
        })
        .collect();
    state
        .service
        .replace_answers_for_user(identity.account.user_id, input)
        .await
        .map(|answers| Json(AnswersResponse { answers }))
        .map_err(catalog_error)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct AdminQuestionsResponse {
    questions: Vec<AdminProfileQuestion>,
}

#[cfg(feature = "webauthn-probe")]
async fn list_admin_questions(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
) -> Result<Json<AdminQuestionsResponse>, ApiError> {
    state
        .service
        .questions_for_admin()
        .await
        .map(|questions| Json(AdminQuestionsResponse { questions }))
        .map_err(catalog_error)
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
        let number = if value.trim().is_empty() {
            0.0
        } else {
            value
                .trim()
                .parse::<f64>()
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
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedJson(body): ValidatedJson<CreateQuestionBody>,
) -> Result<(StatusCode, Json<AdminProfileQuestion>), ApiError> {
    let display_order = match body.display_order {
        OptionalField::Missing => 100,
        OptionalField::Value(value) => value.0,
        OptionalField::Null => return Err(ApiError::internal()),
    };
    state
        .service
        .create_question(ProfileQuestionInput {
            prompt: body.prompt,
            category: body.category,
            display_order,
        })
        .await
        .map(|question| (StatusCode::CREATED, Json(question)))
        .map_err(catalog_error)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateQuestionBody {
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
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedPath(path): ValidatedPath<QuestionIdPath>,
    ValidatedJson(body): ValidatedJson<UpdateQuestionBody>,
) -> Result<Json<AdminProfileQuestion>, ApiError> {
    let supplied = body.prompt.is_supplied()
        || body.category.is_supplied()
        || body.display_order.is_supplied();
    let display_order = body.display_order.into_option().map(|value| value.0);
    state
        .service
        .update_question(
            path.id,
            ProfileQuestionPatch {
                prompt: body.prompt.into_option(),
                category: body.category.into_option(),
                display_order,
                supplied,
            },
        )
        .await
        .map(Json)
        .map_err(catalog_error)
}

#[cfg(feature = "webauthn-probe")]
async fn delete_question(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<CatalogHttpState>,
    ValidatedPath(path): ValidatedPath<QuestionIdPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .delete_question(path.id)
        .await
        .map_err(catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn valid_uuid_all(value: Uuid) -> bool {
    (1..=8).contains(&value.get_version_num()) && value.get_variant() == Variant::RFC4122
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
