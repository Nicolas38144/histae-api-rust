use super::Language;
use crate::profiles::domain::ConsentType;
use std::collections::HashMap;
use url::Url;

#[derive(Clone, Debug, Default)]
pub struct LegalTranslations {
    pub(crate) urls: HashMap<(ConsentType, Language), Url>,
}
impl LegalTranslations {
    pub fn resolve(
        &self,
        kind: ConsentType,
        primary: &str,
        language: Language,
    ) -> (String, Language) {
        if language == Language::Fr {
            return (primary.to_owned(), Language::Fr);
        }
        if let Some(url) = self.urls.get(&(kind, language)) {
            return (url.to_string(), language);
        }
        if let Some(url) = localized_histae_url(primary, language) {
            return (url, language);
        }
        (primary.to_owned(), Language::Fr)
    }
}

/// Only Histae's localized legal routes are rewritten.
/// Custom document hosts keep their configured URL.
fn localized_histae_url(value: &str, language: Language) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    if !matches!(url.host_str(), Some("www.histae.com" | "histae.com")) {
        return None;
    }
    let original = url.path();
    let mut path = original.trim_start_matches('/').splitn(2, '/');
    let first = path.next().unwrap_or_default();
    let rest = if Language::parse(first).is_some() {
        path.next().unwrap_or_default()
    } else {
        original.trim_start_matches('/')
    };
    if !rest.starts_with("legal/") {
        return None;
    }
    url.set_path(&format!("/{}/{rest}", language.as_str()));
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rewrites_only_histae_legal_routes() {
        let url = "https://www.histae.com/legal/privacy?version=2";
        assert_eq!(
            localized_histae_url(url, Language::It).as_deref(),
            Some("https://www.histae.com/it/legal/privacy?version=2")
        );
        assert!(localized_histae_url("https://other.test/legal/privacy", Language::En).is_none());
        let documents = LegalTranslations::default();
        let kind = ConsentType::LocationConsent;
        assert_eq!(
            documents.resolve(
                kind,
                "https://www.histae.com/fr/legal/location",
                Language::Es
            ),
            (
                "https://www.histae.com/es/legal/location".to_owned(),
                Language::Es
            )
        );
        assert_eq!(
            documents
                .resolve(kind, "https://legal.example.test/location", Language::En)
                .1,
            Language::Fr
        );
    }
}
