use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{
    ConsentRecord, Preferences, PreferencesInput, PresenceInput, ProfileInput, ProfileRecord,
    VersionedConsentChange, WriteOutcome,
};
use crate::config::LegalConfig;
use crate::infra::postgres::DatabaseError;

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
