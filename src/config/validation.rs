use super::{ConfigError, EnvironmentSource, types::*};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    time::Duration,
};
use url::Url;

pub(super) fn parse_environment(value: Option<&str>) -> Result<Environment, ConfigError> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("development") => Ok(Environment::Development),
        Some("test") => Ok(Environment::Test),
        Some("production") => Ok(Environment::Production),
        _ => Err(ConfigError::invalid("ENV")),
    }
}

pub(super) fn integer(
    value: &str,
    name: &'static str,
    min: u64,
    max: u64,
) -> Result<u64, ConfigError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::invalid(name));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= min && *value <= max)
        .ok_or_else(|| ConfigError::invalid(name))
}

pub(super) fn duration(value: &str, name: &'static str) -> Result<Duration, ConfigError> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .ok_or_else(|| ConfigError::invalid(name))?;
    let (amount, unit) = value.split_at(split);
    if amount.is_empty() || !amount.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::invalid(name));
    }
    let amount = amount
        .parse::<u64>()
        .map_err(|_| ConfigError::invalid(name))?;
    let millis = match unit {
        "ms" => Some(amount),
        "s" => amount.checked_mul(1_000),
        "m" => amount.checked_mul(60_000),
        "h" => amount.checked_mul(3_600_000),
        _ => None,
    }
    .ok_or_else(|| ConfigError::invalid(name))?;
    if millis == 0 || millis > 9_007_199_254_740_991 {
        return Err(ConfigError::invalid(name));
    }
    Ok(Duration::from_millis(millis))
}

pub(super) fn ensure_duration(
    value: Duration,
    min: Duration,
    max: Duration,
    name: &'static str,
) -> Result<(), ConfigError> {
    if value < min || value > max {
        Err(ConfigError::invalid(name))
    } else {
        Ok(())
    }
}

pub(super) fn boolean(
    value: Option<&str>,
    fallback: bool,
    name: &'static str,
) -> Result<bool, ConfigError> {
    match value.filter(|value| !value.is_empty()) {
        None => Ok(fallback),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(ConfigError::invalid(name)),
    }
}

pub(super) fn phone_key_bytes(value: &str, name: &'static str) -> Result<Vec<u8>, ConfigError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return (0..64)
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&value[index..index + 2], 16)
                    .map_err(|_| ConfigError::invalid(name))
            })
            .collect();
    }
    if value.len() == 32 {
        Ok(value.as_bytes().to_vec())
    } else {
        Err(ConfigError::invalid(name))
    }
}

pub(super) fn jwt_keys(
    active: &str,
    secret: &str,
    raw: &str,
) -> Result<BTreeMap<String, SecretString>, ConfigError> {
    let valid_kid = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    };
    if !valid_kid(active) || secret.len() < 32 || raw.len() > 16_384 {
        return Err(ConfigError::invalid("JWT_PREVIOUS_KEYS"));
    }
    let Value::Object(previous) =
        serde_json::from_str(raw).map_err(|_| ConfigError::invalid("JWT_PREVIOUS_KEYS"))?
    else {
        return Err(ConfigError::invalid("JWT_PREVIOUS_KEYS"));
    };
    if previous.len() > 4 {
        return Err(ConfigError::invalid("JWT_PREVIOUS_KEYS"));
    }
    let mut keys = BTreeMap::from([(active.to_owned(), SecretString::new(secret.to_owned()))]);
    let mut material = BTreeSet::from([secret.to_owned()]);
    for (kid, value) in previous {
        let Some(value) = value.as_str() else {
            return Err(ConfigError::invalid("JWT_PREVIOUS_KEYS"));
        };
        if !valid_kid(&kid)
            || keys.contains_key(&kid)
            || value.len() < 32
            || !material.insert(value.to_owned())
        {
            return Err(ConfigError::invalid("JWT_PREVIOUS_KEYS"));
        }
        keys.insert(kid, SecretString::new(value.to_owned()));
    }
    Ok(keys)
}

pub(super) fn number(
    value: &str,
    name: &'static str,
    min: f64,
    max: f64,
) -> Result<f64, ConfigError> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && value.bytes().filter(|byte| *byte == b'.').count() <= 1
        && value != ".";
    if !valid {
        return Err(ConfigError::invalid(name));
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number >= min && *number <= max)
        .ok_or_else(|| ConfigError::invalid(name))
}

pub(super) fn http_origin(value: &str, name: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(ConfigError::invalid(name));
    }
    Ok(url)
}

pub(super) fn object_storage_endpoint(value: &str, env: Environment) -> Result<Url, ConfigError> {
    let url = http_origin(value, "OBJECT_STORAGE_ENDPOINT")?;
    if env == Environment::Production && url.scheme() != "https" {
        Err(ConfigError::invalid("OBJECT_STORAGE_ENDPOINT"))
    } else {
        Ok(url)
    }
}

pub(super) fn exact_origin(value: &str, name: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.origin().unicode_serialization() != value {
        return Err(ConfigError::invalid(name));
    }
    Ok(url)
}

pub(super) fn webauthn_origin(value: &str, env: Environment) -> Result<String, ConfigError> {
    let url = exact_origin(value, "ADMIN_WEBAUTHN_ORIGIN")?;
    let local = env != Environment::Production
        && url.scheme() == "http"
        && url.host_str() == Some("localhost");
    if url.scheme() != "https" && !local {
        return Err(ConfigError::invalid("ADMIN_WEBAUTHN_ORIGIN"));
    }
    Ok(url.origin().unicode_serialization())
}

pub(super) fn webauthn_rp_id(
    value: &str,
    host: &str,
    env: Environment,
) -> Result<String, ConfigError> {
    let value = value.trim().to_ascii_lowercase();
    let localhost = env != Environment::Production && value == "localhost";
    let domain = value.len() <= 253
        && value.contains('.')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if (!localhost && !domain) || (host != value && !host.ends_with(&format!(".{value}"))) {
        Err(ConfigError::invalid("ADMIN_WEBAUTHN_RP_ID"))
    } else {
        Ok(value)
    }
}

pub(super) fn https_url(value: &str, name: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.scheme() == "https" {
        Ok(url)
    } else {
        Err(ConfigError::invalid(name))
    }
}

pub(super) fn web_origins(value: &str, env: Environment) -> Result<Vec<String>, ConfigError> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let values: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if values.iter().copied().collect::<BTreeSet<_>>().len() != values.len() {
        return Err(ConfigError::invalid("CORS_ORIGINS"));
    }
    for value in &values {
        let url = exact_origin(value, "CORS_ORIGINS")?;
        if url.scheme() != "https" && (env == Environment::Production || url.scheme() != "http") {
            return Err(ConfigError::invalid("CORS_ORIGINS"));
        }
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

pub(super) fn trust_proxy(value: &str, env: Environment) -> Result<TrustProxy, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "false" => return Ok(TrustProxy::Disabled),
        "true" if env != Environment::Production => return Ok(TrustProxy::All),
        "true" => return Err(ConfigError::invalid("TRUST_PROXY")),
        _ => {}
    }
    let entries: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if entries.is_empty()
        || entries.iter().any(|entry| {
            let mut parts = entry.split('/');
            let address = parts.next().and_then(|value| value.parse::<IpAddr>().ok());
            let prefix = parts.next();
            if parts.next().is_some() || address.is_none() {
                return true;
            }
            prefix.is_some_and(|prefix| {
                prefix.parse::<u8>().ok().is_none_or(|prefix| {
                    prefix
                        > if address.is_some_and(|address| address.is_ipv4()) {
                            32
                        } else {
                            128
                        }
                })
            })
        })
    {
        return Err(ConfigError::invalid("TRUST_PROXY"));
    }
    Ok(TrustProxy::Networks(
        entries.into_iter().map(str::to_owned).collect(),
    ))
}

pub(super) fn storage_region(value: &str) -> Result<String, ConfigError> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Err(ConfigError::invalid("OBJECT_STORAGE_REGION"))
    } else {
        Ok(value.to_owned())
    }
}

pub(super) fn storage_bucket(value: &str) -> Result<String, ConfigError> {
    let ip_like = value.split('.').count() == 4
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    let edge_ok = value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if value.len() < 3
        || value.len() > 63
        || !edge_ok
        || value.contains("..")
        || ip_like
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
    {
        Err(ConfigError::invalid("OBJECT_STORAGE_BUCKET"))
    } else {
        Ok(value.to_owned())
    }
}

pub(super) fn legal_config(
    source: &EnvironmentSource,
    env: Environment,
) -> Result<LegalConfig, ConfigError> {
    let names = [
        "TERMS_OF_SERVICE_VERSION",
        "PRIVACY_POLICY_VERSION",
        "SENSITIVE_DATA_CONSENT_VERSION",
        "LOCATION_CONSENT_VERSION",
        "TERMS_OF_SERVICE_URL",
        "PRIVACY_POLICY_URL",
        "SENSITIVE_DATA_CONSENT_URL",
        "LOCATION_CONSENT_URL",
        "LEGAL_REVIEW_REFERENCE",
    ];
    let mut values = Vec::with_capacity(names.len());
    for name in names {
        values.push(source.or(name, "")?);
    }
    if env == Environment::Production && values.iter().any(String::is_empty) {
        return Err(ConfigError::missing("LEGAL_CONFIGURATION"));
    }
    let fallback = "https://example.invalid/histae/legal";
    Ok(LegalConfig {
        terms_version: fallback_value(&values[0], "development-unversioned"),
        privacy_version: fallback_value(&values[1], "development-unversioned"),
        sensitive_data_consent_version: fallback_value(&values[2], "development-unversioned"),
        location_consent_version: fallback_value(&values[3], "development-unversioned"),
        terms_url: legal_url(
            &fallback_value(&values[4], &format!("{fallback}/terms")),
            "TERMS_OF_SERVICE_URL",
            env,
        )?,
        privacy_url: legal_url(
            &fallback_value(&values[5], &format!("{fallback}/privacy")),
            "PRIVACY_POLICY_URL",
            env,
        )?,
        sensitive_data_consent_url: legal_url(
            &fallback_value(&values[6], &format!("{fallback}/sensitive-data")),
            "SENSITIVE_DATA_CONSENT_URL",
            env,
        )?,
        location_consent_url: legal_url(
            &fallback_value(&values[7], &format!("{fallback}/location")),
            "LOCATION_CONSENT_URL",
            env,
        )?,
        review_reference: fallback_value(&values[8], "not-reviewed-for-production"),
    })
}

pub(super) fn fallback_value(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

pub(super) fn legal_url(
    value: &str,
    name: &'static str,
    env: Environment,
) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.scheme() != "https" && (env == Environment::Production || url.scheme() != "http") {
        Err(ConfigError::invalid(name))
    } else {
        Ok(url)
    }
}

pub(super) fn billing_config(
    source: &EnvironmentSource,
    env: Environment,
) -> Result<BillingConfig, ConfigError> {
    let provider = match source
        .or(
            "BILLING_PROVIDER",
            if env == Environment::Production {
                "stripe"
            } else {
                "disabled"
            },
        )?
        .as_str()
    {
        "disabled" => BillingProvider::Disabled,
        "stripe" => BillingProvider::Stripe,
        _ => return Err(ConfigError::invalid("BILLING_PROVIDER")),
    };
    if env == Environment::Production && provider != BillingProvider::Stripe {
        return Err(ConfigError::invalid("BILLING_PROVIDER"));
    }
    let secret = source.or("STRIPE_SECRET_KEY", "")?;
    let webhook = source.or("STRIPE_WEBHOOK_SECRET", "")?;
    let product = source.or("STRIPE_PREMIUM_PRODUCT_ID", "")?;
    let monthly = source.or("STRIPE_PREMIUM_MONTHLY_PRICE_ID", "")?;
    let annual = source.or("STRIPE_PREMIUM_ANNUAL_PRICE_ID", "")?;
    let id_suffix = |value: &str, prefix: &str| {
        value.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    };
    if provider == BillingProvider::Stripe {
        if !(id_suffix(&secret, "sk_test_") || id_suffix(&secret, "sk_live_")) {
            return Err(ConfigError::invalid("STRIPE_SECRET_KEY"));
        }
        if env == Environment::Production && !secret.starts_with("sk_live_") {
            return Err(ConfigError::invalid("STRIPE_SECRET_KEY"));
        }
        if env != Environment::Production && !secret.starts_with("sk_test_") {
            return Err(ConfigError::invalid("STRIPE_SECRET_KEY"));
        }
        if !id_suffix(&webhook, "whsec_") {
            return Err(ConfigError::invalid("STRIPE_WEBHOOK_SECRET"));
        }
        if !id_suffix(&product, "prod_") {
            return Err(ConfigError::invalid("STRIPE_PREMIUM_PRODUCT_ID"));
        }
        if !id_suffix(&monthly, "price_") || !id_suffix(&annual, "price_") || monthly == annual {
            return Err(ConfigError::invalid("STRIPE_PRICE_IDS"));
        }
    }
    let timeout = duration(&source.or("STRIPE_TIMEOUT", "10s")?, "STRIPE_TIMEOUT")?;
    if timeout > Duration::from_secs(30) {
        return Err(ConfigError::invalid("STRIPE_TIMEOUT"));
    }
    let interval = duration(
        &source.or("STRIPE_RECONCILIATION_INTERVAL", "5m")?,
        "STRIPE_RECONCILIATION_INTERVAL",
    )?;
    ensure_duration(
        interval,
        Duration::from_secs(60),
        Duration::from_secs(86_400),
        "STRIPE_RECONCILIATION_INTERVAL",
    )?;
    let freshness = duration(
        &source.or("STRIPE_RECONCILIATION_FRESHNESS", "1h")?,
        "STRIPE_RECONCILIATION_FRESHNESS",
    )?;
    if freshness < interval || freshness > Duration::from_secs(604_800) {
        return Err(ConfigError::invalid("STRIPE_RECONCILIATION_FRESHNESS"));
    }
    let success = source.or("STRIPE_CHECKOUT_SUCCESS_URL", "")?;
    let cancel = source.or("STRIPE_CHECKOUT_CANCEL_URL", "")?;
    let portal = source.or("STRIPE_PORTAL_RETURN_URL", "")?;
    let (success, cancel, portal) = if provider == BillingProvider::Stripe {
        if !success.contains("{CHECKOUT_SESSION_ID}") {
            return Err(ConfigError::invalid("STRIPE_CHECKOUT_SUCCESS_URL"));
        }
        https_url(
            &success.replace("{CHECKOUT_SESSION_ID}", "cs_test_validation"),
            "STRIPE_CHECKOUT_SUCCESS_URL",
        )?;
        https_url(&cancel, "STRIPE_CHECKOUT_CANCEL_URL")?;
        https_url(&portal, "STRIPE_PORTAL_RETURN_URL")?;
        (Some(success), Some(cancel), Some(portal))
    } else {
        (None, None, None)
    };
    Ok(BillingConfig {
        provider,
        stripe_secret_key: SecretString::new(secret),
        stripe_webhook_secret: SecretString::new(webhook),
        premium_product_id: product,
        premium_monthly_price_id: monthly,
        premium_annual_price_id: annual,
        checkout_success_url: success,
        checkout_cancel_url: cancel,
        portal_return_url: portal,
        automatic_tax: boolean(
            source.value("STRIPE_AUTOMATIC_TAX")?.as_deref(),
            false,
            "STRIPE_AUTOMATIC_TAX",
        )?,
        allow_promotion_codes: boolean(
            source.value("STRIPE_ALLOW_PROMOTION_CODES")?.as_deref(),
            false,
            "STRIPE_ALLOW_PROMOTION_CODES",
        )?,
        timeout,
        max_network_retries: integer(
            &source.or("STRIPE_MAX_NETWORK_RETRIES", "2")?,
            "STRIPE_MAX_NETWORK_RETRIES",
            0,
            5,
        )? as u8,
        reconciliation_interval: interval,
        reconciliation_freshness: freshness,
        reconciliation_batch_size: integer(
            &source.or("STRIPE_RECONCILIATION_BATCH_SIZE", "25")?,
            "STRIPE_RECONCILIATION_BATCH_SIZE",
            1,
            100,
        )? as u16,
    })
}

pub(super) fn limit(
    source: &EnvironmentSource,
    prefix: &'static str,
    default_max: u32,
    default_window: &str,
) -> Result<LimitPolicy, ConfigError> {
    let window_name: &'static str = match prefix {
        "RATE_LIMIT_GLOBAL" => "RATE_LIMIT_GLOBAL_WINDOW",
        "RATE_LIMIT_OTP" => "RATE_LIMIT_OTP_WINDOW",
        "RATE_LIMIT_REFRESH" => "RATE_LIMIT_REFRESH_WINDOW",
        "RATE_LIMIT_FEED" => "RATE_LIMIT_FEED_WINDOW",
        "RATE_LIMIT_MESSAGE" => "RATE_LIMIT_MESSAGE_WINDOW",
        "RATE_LIMIT_DATA_EXPORT" => "RATE_LIMIT_DATA_EXPORT_WINDOW",
        "RATE_LIMIT_REPORT" => "RATE_LIMIT_REPORT_WINDOW",
        "RATE_LIMIT_PHOTO" => "RATE_LIMIT_PHOTO_WINDOW",
        "RATE_LIMIT_SWIPE" => "RATE_LIMIT_SWIPE_WINDOW",
        "RATE_LIMIT_BILLING" => "RATE_LIMIT_BILLING_WINDOW",
        "RATE_LIMIT_BILLING_WEBHOOK" => "RATE_LIMIT_BILLING_WEBHOOK_WINDOW",
        "RATE_LIMIT_SMS_WEBHOOK" => "RATE_LIMIT_SMS_WEBHOOK_WINDOW",
        "RATE_LIMIT_ADMIN_AUTH" => "RATE_LIMIT_ADMIN_AUTH_WINDOW",
        _ => return Err(ConfigError::invalid("RATE_LIMIT")),
    };
    Ok(LimitPolicy {
        max: integer(
            &source.or(prefix, &default_max.to_string())?,
            prefix,
            1,
            9_007_199_254_740_991,
        )?,
        window: duration(&source.or(window_name, default_window)?, window_name)?,
    })
}
