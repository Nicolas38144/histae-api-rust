use super::{
    ConfigError, Environment, EnvironmentSource, SecretString, StoreBillingConfig,
    validation::boolean,
};
use std::path::PathBuf;
use url::Url;

pub(super) fn load(
    source: &EnvironmentSource,
    environment: Environment,
) -> Result<StoreBillingConfig, ConfigError> {
    let flag = |name| boolean(source.value(name)?.as_deref(), false, name);
    let apple_enabled = flag("APP_STORE_ENABLED")?;
    let google_enabled = flag("GOOGLE_PLAY_ENABLED")?;
    let required = |name, enabled| {
        let value = source.or(name, "")?;
        if enabled && value.is_empty() {
            Err(ConfigError::missing(name))
        } else {
            Ok(value)
        }
    };
    let apple_monthly_product = required("APP_STORE_PREMIUM_MONTHLY_PRODUCT_ID", apple_enabled)?;
    let apple_annual_product = required("APP_STORE_PREMIUM_ANNUAL_PRODUCT_ID", apple_enabled)?;
    let google_product = required("GOOGLE_PLAY_PREMIUM_PRODUCT_ID", google_enabled)?;
    let google_monthly_plan = required("GOOGLE_PLAY_MONTHLY_BASE_PLAN_ID", google_enabled)?;
    let google_annual_plan = required("GOOGLE_PLAY_ANNUAL_BASE_PLAN_ID", google_enabled)?;
    if apple_enabled && apple_monthly_product == apple_annual_product {
        return Err(ConfigError::invalid("APP_STORE_PREMIUM_ANNUAL_PRODUCT_ID"));
    }
    if google_enabled && google_monthly_plan == google_annual_plan {
        return Err(ConfigError::invalid("GOOGLE_PLAY_ANNUAL_BASE_PLAN_ID"));
    }
    let token = required("STORE_VERIFIER_TOKEN", apple_enabled || google_enabled)?;
    if (apple_enabled || google_enabled) && token.len() < 32 {
        return Err(ConfigError::invalid("STORE_VERIFIER_TOKEN"));
    }
    let production = environment == Environment::Production;
    let default = if production {
        "https://store-verifier:8080"
    } else {
        "http://store-verifier:8080"
    };
    let verifier_url = Url::parse(&source.or("STORE_VERIFIER_URL", default)?)
        .map_err(|_| ConfigError::invalid("STORE_VERIFIER_URL"))?;
    if !matches!(verifier_url.scheme(), "http" | "https")
        || verifier_url.host_str().is_none()
        || !verifier_url.username().is_empty()
        || verifier_url.password().is_some()
        || verifier_url.query().is_some()
        || verifier_url.fragment().is_some()
        || verifier_url.path() != "/"
    {
        return Err(ConfigError::invalid("STORE_VERIFIER_URL"));
    }
    let verifier_ca_file = source
        .value("STORE_VERIFIER_CA_FILE")?
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if (production || verifier_ca_file.is_some()) && verifier_url.scheme() != "https" {
        return Err(ConfigError::invalid("STORE_VERIFIER_URL"));
    }
    if verifier_ca_file
        .as_ref()
        .is_some_and(|file| !file.is_absolute())
    {
        return Err(ConfigError::invalid("STORE_VERIFIER_CA_FILE"));
    }
    if production && (apple_enabled || google_enabled) && verifier_ca_file.is_none() {
        return Err(ConfigError::missing("STORE_VERIFIER_CA_FILE"));
    }
    Ok(StoreBillingConfig {
        apple_enabled,
        google_enabled,
        apple_monthly_product,
        apple_annual_product,
        google_product,
        google_monthly_plan,
        google_annual_plan,
        verifier_url,
        verifier_ca_file,
        verifier_token: SecretString::new(token),
        production,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_requires_https_and_a_ca_for_active_payments() {
        let source = |url, ca| {
            EnvironmentSource::from_pairs([
                ("APP_STORE_ENABLED", "true"),
                ("APP_STORE_PREMIUM_MONTHLY_PRODUCT_ID", "test-monthly"),
                ("APP_STORE_PREMIUM_ANNUAL_PRODUCT_ID", "test-annual"),
                (
                    "STORE_VERIFIER_TOKEN",
                    "synthetic-credential-for-tests-only",
                ),
                ("STORE_VERIFIER_URL", url),
                ("STORE_VERIFIER_CA_FILE", ca),
            ])
        };
        let ca = "/tmp/ca.crt";
        assert_eq!(
            load(
                &source("http://store-verifier:8080", ca),
                Environment::Production
            )
            .unwrap_err()
            .variable,
            "STORE_VERIFIER_URL"
        );
        assert_eq!(
            load(
                &source("https://store-verifier:8080", ""),
                Environment::Production
            )
            .unwrap_err()
            .variable,
            "STORE_VERIFIER_CA_FILE"
        );
        assert!(
            load(
                &source("https://store-verifier:8080", ca),
                Environment::Production
            )
            .is_ok()
        );
    }
}
