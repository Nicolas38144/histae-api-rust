use crate::{
    billing::{domain::SubscriptionView, service::BillingError},
    infra::postgres::DatabaseError,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

pub struct WebChallenge {
    pub id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub matched: bool,
    pub created: bool,
}
pub struct WebIdentity {
    pub user_id: Uuid,
    pub onboarding_complete: bool,
}
#[derive(Serialize)]
pub struct ChallengeView {
    pub challenge_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub message: &'static str,
}
#[derive(Serialize)]
pub struct WebSessionView {
    pub user_id: Uuid,
    pub onboarding_complete: bool,
    pub subscription: SubscriptionView,
}
#[derive(Debug)]
pub enum WebBillingError {
    InvalidRequest,
    Authentication,
    IdempotencyConflict,
    OnboardingIncomplete,
    Internal,
    Database(DatabaseError),
    Billing(BillingError),
}
impl From<DatabaseError> for WebBillingError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}
impl From<BillingError> for WebBillingError {
    fn from(error: BillingError) -> Self {
        Self::Billing(error)
    }
}
