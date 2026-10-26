use crate::config::LegalConfig;
use crate::localization::{Language, legal::LegalTranslations};

use super::domain::{ConsentType, PublicLegalDocument};

/// Immutable document metadata, independent of accounts and consent evidence.
#[derive(Clone)]
pub(super) struct LegalDocumentCatalog {
    config: LegalConfig,
    translations: LegalTranslations,
}

impl LegalDocumentCatalog {
    pub(super) fn new(config: LegalConfig) -> Self {
        Self {
            config,
            translations: LegalTranslations::default(),
        }
    }

    pub(super) fn with_translations(mut self, translations: LegalTranslations) -> Self {
        self.translations = translations;
        self
    }

    pub(super) fn config(&self) -> &LegalConfig {
        &self.config
    }

    pub(super) fn document(
        &self,
        consent_type: ConsentType,
        language: Language,
    ) -> PublicLegalDocument {
        let (document_url, document_language) = self.translations.resolve(
            consent_type,
            &consent_type.document_url(&self.config),
            language,
        );
        PublicLegalDocument {
            consent_type,
            required_document_version: consent_type.version(&self.config).to_owned(),
            document_url,
            document_language,
            document_date: consent_type.document_date(&self.config),
        }
    }

    pub(super) fn documents(&self, language: Language) -> Vec<PublicLegalDocument> {
        ConsentType::ALL
            .into_iter()
            .map(|kind| self.document(kind, language))
            .collect()
    }
}
