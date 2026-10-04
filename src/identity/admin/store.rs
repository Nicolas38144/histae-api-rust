use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::postgres::DatabaseError;

use super::domain::{
    ActiveSessionRow, AuthEventRow, BootstrapRow, ChallengePurpose, ChallengeRow,
    CredentialRevocation, CredentialRow, EventCursor, NewCredential, NewSession, SessionSummaryRow,
};

pub type AdminStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct NewChallenge {
    pub purpose: ChallengePurpose,
    pub challenge_hash: [u8; 32],
    pub ceremony_state: Vec<u8>,
    pub user_id: Option<Uuid>,
    pub bootstrap_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
}

pub trait AdminAuthStore: Send + Sync {
    fn bootstrap(
        &self,
        id: Uuid,
        secret_hash: [u8; 32],
    ) -> AdminStoreFuture<'_, Option<BootstrapRow>>;
    fn create_challenge(&self, challenge: NewChallenge) -> AdminStoreFuture<'_, Uuid>;
    fn consume_challenge(
        &self,
        id: Uuid,
        purpose: ChallengePurpose,
        user_id: Option<Uuid>,
        bootstrap_id: Option<Uuid>,
    ) -> AdminStoreFuture<'_, Option<ChallengeRow>>;
    fn active_credentials(&self, user_id: Uuid) -> AdminStoreFuture<'_, Vec<CredentialRow>>;
    fn active_credential_by_external_id(
        &self,
        credential_id: String,
    ) -> AdminStoreFuture<'_, Option<CredentialRow>>;
    fn complete_bootstrap(
        &self,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        credential: NewCredential,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn add_credential(
        &self,
        user_id: Uuid,
        credential: NewCredential,
    ) -> AdminStoreFuture<'_, bool>;
    fn complete_authentication(
        &self,
        credential_id: Uuid,
        expected_counter: u32,
        next_counter: u32,
        device_type: String,
        backed_up: bool,
        session: NewSession,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn active_session(
        &self,
        token_hash: [u8; 32],
        idle_ttl_millis: i64,
    ) -> AdminStoreFuture<'_, Option<ActiveSessionRow>>;
    fn revoke_session(&self, user_id: Uuid, session_id: Uuid) -> AdminStoreFuture<'_, ()>;
    fn revoke_other_sessions(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, u64>;
    fn active_sessions(&self, user_id: Uuid) -> AdminStoreFuture<'_, Vec<SessionSummaryRow>>;
    fn revoke_selected_session(
        &self,
        user_id: Uuid,
        target_session_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, bool>;
    fn rename_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
        name: String,
    ) -> AdminStoreFuture<'_, bool>;
    fn auth_events(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<EventCursor>,
    ) -> AdminStoreFuture<'_, Vec<AuthEventRow>>;
    fn revoke_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
    ) -> AdminStoreFuture<'_, CredentialRevocation>;
    fn issue_bootstrap(
        &self,
        user_id: Uuid,
        bootstrap_id: Uuid,
        secret_hash: [u8; 32],
        expires_at: DateTime<Utc>,
    ) -> AdminStoreFuture<'_, bool>;
}
