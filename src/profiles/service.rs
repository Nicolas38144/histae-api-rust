use crate::localization::{Language, legal::LegalTranslations};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{NaiveDate, SecondsFormat};
use uuid::Uuid;

use super::domain::{
    ConsentChange, ConsentState, ConsentType, LookingFor, PreferencesInput, PresenceInput,
    ProfileInput, PublicConsent, PublicModeration, PublicModerationState, PublicPreferences,
    PublicProfile, Sex, VersionedConsentChange, WriteOutcome,
};
use super::legal::LegalDocumentCatalog;
use super::store::ProfileStore;
use crate::config::LegalConfig;
use crate::infra::postgres::DatabaseError;
use crate::moderation::text::TextModerator;
use crate::shared::clock::Clock;
use crate::shared::text::{javascript_trim, utf8_len};

pub type ProfilePhotoUrlFuture<'provider> =
    Pin<Box<dyn Future<Output = Result<Option<String>, ()>> + Send + 'provider>>;

pub trait ProfilePhotoUrlProvider: Send + Sync {
    fn url_for_key(&self, object_key: Option<String>) -> ProfilePhotoUrlFuture<'_>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    InvalidProfile,
    ProfileNotFound,
    InvalidPreferences,
    PreferencesNotFound,
    InvalidPresence,
    PresenceUpdateLimited,
    InvalidConsentPayload,
    RequiredConsentMissing,
    AccountNotFound,
    Database(DatabaseError),
    PhotoUrlUnavailable,
}

impl From<DatabaseError> for ProfileError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

#[derive(Clone)]
pub struct ProfileService {
    store: Arc<dyn ProfileStore>,
    legal: LegalDocumentCatalog,
    clock: Arc<dyn Clock>,
    photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
    moderator: TextModerator,
}

impl ProfileService {
    pub fn new(
        store: Arc<dyn ProfileStore>,
        legal: LegalConfig,
        clock: Arc<dyn Clock>,
        photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
    ) -> Self {
        Self {
            store,
            legal: LegalDocumentCatalog::new(legal),
            clock,
            photo_urls,
            moderator: TextModerator,
        }
    }

    pub fn with_legal_translations(mut self, documents: LegalTranslations) -> Self {
        self.legal = self.legal.with_translations(documents);
        self
    }

    pub async fn profile(&self, user_id: Uuid) -> Result<PublicProfile, ProfileError> {
        let row = self
            .store
            .find_profile(user_id)
            .await?
            .ok_or(ProfileError::ProfileNotFound)?;
        let photo = self
            .photo_urls
            .url_for_key(row.photo_object_key.clone())
            .await
            .map_err(|()| ProfileError::PhotoUrlUnavailable)?;
        Ok(PublicProfile {
            user_id: row.user_id,
            firstname: row.firstname,
            birthdate: row.birthdate,
            sex: row.sex,
            bio: row.bio,
            photo,
            profile_answers: row.profile_answers,
            moderation: PublicModeration {
                bio: row
                    .bio_moderation_status
                    .map(|status| PublicModerationState {
                        status,
                        reasons: row.bio_moderation_reasons,
                    }),
                photo: row
                    .photo_moderation_status
                    .map(|status| PublicModerationState {
                        status,
                        reasons: row.photo_moderation_reasons,
                    }),
            },
        })
    }

    pub async fn update_profile(
        &self,
        user_id: Uuid,
        firstname: String,
        birthdate: String,
        sex: Option<Sex>,
        bio: Option<String>,
    ) -> Result<(), ProfileError> {
        let firstname = javascript_trim(&firstname).to_owned();
        let birthdate = strict_birthdate(&birthdate).ok_or(ProfileError::InvalidProfile)?;
        if firstname.is_empty()
            || utf8_len(&firstname) > 100
            || !is_adult(birthdate, self.clock.today_utc())
        {
            return Err(ProfileError::InvalidProfile);
        }
        let bio = bio.map(|value| javascript_trim(&value).to_owned());
        if bio.as_ref().is_some_and(|value| utf8_len(value) > 2_000) {
            return Err(ProfileError::InvalidProfile);
        }
        let bio_moderation = bio
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| self.moderator.analyze(value));
        self.apply_write(
            self.store
                .upsert_profile(
                    user_id,
                    ProfileInput {
                        firstname,
                        birthdate,
                        sex,
                        bio,
                        bio_moderation,
                    },
                    self.legal.config().clone(),
                )
                .await?,
        )
    }

    pub async fn preferences(&self, user_id: Uuid) -> Result<PublicPreferences, ProfileError> {
        self.store
            .find_preferences(user_id)
            .await?
            .map(Into::into)
            .ok_or(ProfileError::PreferencesNotFound)
    }

    pub async fn update_preferences(
        &self,
        user_id: Uuid,
        min_age: f64,
        max_age: f64,
        max_distance_km: f64,
        looking_for: LookingFor,
    ) -> Result<(), ProfileError> {
        if !valid_integer(min_age)
            || !valid_integer(max_age)
            || !valid_integer(max_distance_km)
            || min_age < 18.0
            || max_age < min_age
            || max_age > 99.0
            || !(1.0..=500.0).contains(&max_distance_km)
        {
            return Err(ProfileError::InvalidPreferences);
        }
        self.apply_write(
            self.store
                .upsert_preferences(
                    user_id,
                    PreferencesInput {
                        min_age: min_age as i32,
                        max_age: max_age as i32,
                        max_distance_km: max_distance_km as i32,
                        looking_for,
                    },
                    self.legal.config().clone(),
                )
                .await?,
        )
    }

    pub async fn update_presence(
        &self,
        user_id: Uuid,
        latitude: f64,
        longitude: f64,
    ) -> Result<(), ProfileError> {
        if !latitude.is_finite()
            || !longitude.is_finite()
            || !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
        {
            return Err(ProfileError::InvalidPresence);
        }
        let (latitude, longitude) = super::location::coarse_coordinates(latitude, longitude);
        self.apply_write(
            self.store
                .upsert_presence(
                    user_id,
                    PresenceInput {
                        latitude,
                        longitude,
                        updated_at: self.clock.now(),
                    },
                    self.legal.config().clone(),
                )
                .await?,
        )
    }

    pub async fn consents(&self, user_id: Uuid) -> Result<ConsentState, ProfileError> {
        self.consents_in(user_id, Language::Fr).await
    }

    pub(super) fn legal_documents(&self) -> LegalDocumentCatalog {
        self.legal.clone()
    }

    pub async fn consents_in(
        &self,
        user_id: Uuid,
        language: Language,
    ) -> Result<ConsentState, ProfileError> {
        let current = self.store.current_consents(user_id).await?;
        let by_type = current
            .into_iter()
            .map(|consent| (consent.consent_type, consent))
            .collect::<HashMap<_, _>>();
        let required_actions = ConsentType::ONBOARDING
            .into_iter()
            .filter(|consent_type| {
                by_type.get(consent_type).is_none_or(|consent| {
                    !consent.granted
                        || consent.document_version != consent_type.version(self.legal.config())
                })
            })
            .collect::<Vec<_>>();
        let consents = self
            .legal
            .documents(language)
            .into_iter()
            .map(|document| {
                let consent_type = document.consent_type;
                let current = by_type.get(&consent_type);
                PublicConsent {
                    consent_type,
                    granted: current.is_some_and(|consent| consent.granted),
                    document_version: current.map(|consent| consent.document_version.clone()),
                    required_document_version: document.required_document_version,
                    document_url: document.document_url,
                    document_language: document.document_language,
                    accepted_document_language: current.map(|consent| consent.document_language),
                    updated_at: current.map(|consent| {
                        consent
                            .granted_at
                            .to_rfc3339_opts(SecondsFormat::Millis, true)
                    }),
                }
            })
            .collect();
        Ok(ConsentState {
            consents,
            onboarding_complete: required_actions.is_empty(),
            required_actions,
        })
    }

    pub async fn update_consents(
        &self,
        user_id: Uuid,
        changes: Vec<ConsentChange>,
        ip_address: String,
        user_agent: String,
    ) -> Result<ConsentState, ProfileError> {
        self.update_consents_in(user_id, changes, ip_address, user_agent, Language::Fr)
            .await
    }

    pub async fn update_consents_in(
        &self,
        user_id: Uuid,
        changes: Vec<ConsentChange>,
        ip_address: String,
        user_agent: String,
        language: Language,
    ) -> Result<ConsentState, ProfileError> {
        let unique = changes
            .iter()
            .map(|change| change.consent_type)
            .collect::<HashSet<_>>();
        if changes.is_empty()
            || unique.len() != changes.len()
            || changes
                .iter()
                .any(|change| !change.granted && change.consent_type.is_non_withdrawable())
        {
            return Err(ProfileError::InvalidConsentPayload);
        }
        let versioned = changes
            .into_iter()
            .map(|change| {
                let document = self.legal.document(change.consent_type, language);
                VersionedConsentChange {
                    consent_type: change.consent_type,
                    granted: change.granted,
                    document_version: document.required_document_version,
                    document_language: document.document_language,
                }
            })
            .collect();
        if !self
            .store
            .record_consents(user_id, versioned, ip_address, user_agent)
            .await?
        {
            return Err(ProfileError::AccountNotFound);
        }
        self.consents_in(user_id, language).await
    }

    fn apply_write(&self, outcome: WriteOutcome) -> Result<(), ProfileError> {
        match outcome {
            WriteOutcome::Updated => Ok(()),
            WriteOutcome::AccountNotFound => Err(ProfileError::AccountNotFound),
            WriteOutcome::RequiredConsentMissing => Err(ProfileError::RequiredConsentMissing),
            WriteOutcome::PresenceUpdateLimited => Err(ProfileError::PresenceUpdateLimited),
        }
    }
}

fn strict_birthdate(value: &str) -> Option<NaiveDate> {
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value
            .bytes()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn is_adult(birthdate: NaiveDate, today: NaiveDate) -> bool {
    let threshold_year = today.year() - 18;
    birthdate.year() < threshold_year
        || (birthdate.year() == threshold_year
            && (birthdate.month(), birthdate.day()) <= (today.month(), today.day()))
}

fn valid_integer(value: f64) -> bool {
    value.is_finite()
        && value.fract() == 0.0
        && value >= i32::MIN as f64
        && value <= i32::MAX as f64
}

use chrono::Datelike as _;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_calendar_dates_and_the_utc_eighteenth_birthday() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 21).expect("valid date");
        assert!(is_adult(
            strict_birthdate("2008-09-21").expect("valid date"),
            today
        ));
        assert!(!is_adult(
            strict_birthdate("2008-09-22").expect("valid date"),
            today
        ));
        for invalid in [
            "2000-02-30",
            "2001-13-01",
            "2001-00-10",
            "2001-01-00",
            "2001-1-01",
            "2001-01-01T00:00:00Z",
        ] {
            assert_eq!(strict_birthdate(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn accepts_json_numbers_that_are_mathematical_integers() {
        assert!(valid_integer(25.0));
        assert!(!valid_integer(25.5));
        assert!(!valid_integer(f64::INFINITY));
    }
}
