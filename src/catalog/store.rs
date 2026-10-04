use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{
    AdminProfileQuestionRow, PlanRow, PreparedProfileAnswer, ProfileAnswer, ProfileQuestion,
    ProfileQuestionInput, ProfileQuestionPatch, ReplaceAnswersOutcome, Trait,
};
use crate::infra::postgres::DatabaseError;

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
