use super::{
    ConfigError, Environment, EnvironmentSource, SecretString, StoreBillingConfig,
    validation::boolean,
};
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
    let verifier_url = Url::parse(&source.or("STORE_VERIFIER_URL", "http://store-verifier:8080")?)
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
    Ok(StoreBillingConfig {
        apple_enabled,
        google_enabled,
        apple_monthly_product,
        apple_annual_product,
        google_product,
        google_monthly_plan,
        google_annual_plan,
        verifier_url,
        verifier_token: SecretString::new(token),
        production: environment == Environment::Production,
    })
}
