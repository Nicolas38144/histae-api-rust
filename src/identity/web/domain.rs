use crate::infra::postgres::DatabaseError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

pub struct WebChallenge {
    pub id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub matched: bool,
    pub created: bool,
}
#[derive(Serialize)]
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
#[derive(Debug)]
pub enum WebAccountError {
    InvalidRequest,
    Authentication,
    IdempotencyConflict,
    Internal,
    Database(DatabaseError),
}
impl From<DatabaseError> for WebAccountError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}
