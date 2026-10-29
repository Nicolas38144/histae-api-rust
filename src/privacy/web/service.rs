use serde::Deserialize;
use uuid::Uuid;

use crate::{
    localization::Language,
    media::service::{PhotoError, PhotoService},
    profiles::{
        domain::{ConsentChange, ConsentType},
        service::{ProfileError, ProfileService},
    },
};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DataCategory {
    Photo,
    Location,
    SensitiveProfile,
}

pub(super) enum DataDeletionError {
    Photo(PhotoError),
    Profile(ProfileError),
}

#[derive(Clone)]
pub struct WebDataDeletionService {
    photos: PhotoService,
    profiles: ProfileService,
}

impl WebDataDeletionService {
    pub fn new(photos: PhotoService, profiles: ProfileService) -> Self {
        Self { photos, profiles }
    }

    pub(super) async fn delete(
        &self,
        user_id: Uuid,
        category: DataCategory,
        ip_address: String,
        user_agent: String,
        language: Language,
    ) -> Result<(), DataDeletionError> {
        let consent_type = match category {
            DataCategory::Photo => {
                return self
                    .photos
                    .delete(user_id)
                    .await
                    .map_err(DataDeletionError::Photo);
            }
            DataCategory::Location => ConsentType::LocationConsent,
            DataCategory::SensitiveProfile => ConsentType::SensitiveDataConsent,
        };
        self.profiles
            .update_consents_in(
                user_id,
                vec![ConsentChange {
                    consent_type,
                    granted: false,
                }],
                ip_address,
                user_agent,
                language,
            )
            .await
            .map_err(DataDeletionError::Profile)?;
        Ok(())
    }
}
