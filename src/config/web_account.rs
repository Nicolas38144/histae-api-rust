use super::{ConfigError, Environment, EnvironmentSource, WebAccountConfig};
use url::Url;

pub(super) fn load(
    source: &EnvironmentSource,
    environment: Environment,
) -> Result<WebAccountConfig, ConfigError> {
    let origin = source.or(
        "WEB_ACCOUNT_ORIGIN",
        if environment == Environment::Production {
            "https://www.histae.com"
        } else {
            "http://localhost:3000"
        },
    )?;
    let url = Url::parse(&origin).map_err(|_| ConfigError::invalid("WEB_ACCOUNT_ORIGIN"))?;
    if url.origin().ascii_serialization() != origin
        || (environment == Environment::Production && url.scheme() != "https")
        || !matches!(url.scheme(), "http" | "https")
    {
        return Err(ConfigError::invalid("WEB_ACCOUNT_ORIGIN"));
    }
    Ok(WebAccountConfig {
        origin,
        secure_cookie: url.scheme() == "https",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn website_sessions_are_configured_independently_of_purchases() {
        let source = EnvironmentSource::default();
        let config = load(&source, Environment::Production).unwrap();
        assert_eq!(config.origin, "https://www.histae.com");
        assert!(config.secure_cookie);

        let source = EnvironmentSource::from_pairs([(
            "WEB_ACCOUNT_ORIGIN",
            "https://account.example.test:8443",
        )]);
        let config = load(&source, Environment::Development).unwrap();
        assert_eq!(config.origin, "https://account.example.test:8443");
        assert!(config.secure_cookie);
    }

    #[test]
    fn website_origin_rejects_insecure_production_and_non_origin_urls() {
        for origin in [
            "http://www.histae.com",
            "https://www.histae.com/",
            "https://www.histae.com/account",
            "https://www.histae.com?redirect=evil",
            "https://www.histae.com#fragment",
            "https://user:password@www.histae.com",
            "https://www.histae.com:443",
            "file:///account",
            "null",
        ] {
            let source = EnvironmentSource::from_pairs([("WEB_ACCOUNT_ORIGIN", origin)]);
            let error = load(&source, Environment::Production).unwrap_err();
            assert_eq!(error.variable, "WEB_ACCOUNT_ORIGIN", "{origin}");
        }
    }
}
