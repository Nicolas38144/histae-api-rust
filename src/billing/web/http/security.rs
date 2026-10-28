use crate::{config::WebBillingConfig, http::error::ApiError};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
pub(crate) fn validate_origin(
    config: &WebBillingConfig,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if !config.enabled {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "web_billing_unavailable",
            "Website billing is unavailable.",
        ));
    }
    validate_website_origin(config, headers)
}

// Account privacy remains available when commercial website billing is disabled.
pub(crate) fn validate_website_origin(
    config: &WebBillingConfig,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            != Some(config.origin.as_str())
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_web_request_origin",
            "The website request origin is not allowed.",
        ));
    }
    Ok(())
}
fn cookie_name(config: &WebBillingConfig) -> &'static str {
    if config.secure_cookie {
        "__Host-histae_billing"
    } else {
        "histae_billing"
    }
}
pub(crate) fn token(headers: &HeaderMap, config: &WebBillingConfig) -> Option<String> {
    if headers.get_all(header::COOKIE).iter().count() != 1 {
        return None;
    }
    let mut values = headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(name, _)| *name == cookie_name(config));
    let (_, token) = values.next()?;
    if values.next().is_some()
        || token.len() != 43
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(token.to_owned())
}
pub(crate) fn set_cookie(
    config: &WebBillingConfig,
    token: &str,
    expiry: u16,
) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={expiry}{}",
        cookie_name(config),
        if config.secure_cookie { "; Secure" } else { "" }
    ))
    .map_err(|_| ApiError::internal())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> WebBillingConfig {
        WebBillingConfig {
            enabled: true,
            origin: "https://www.histae.com".into(),
            secure_cookie: true,
        }
    }
    #[test]
    fn cookie_is_host_scoped_secure_http_only_and_ambiguous_cookies_are_rejected() {
        let config = config();
        let secret = "a".repeat(43);
        let cookie = set_cookie(&config, &secret, 1800).unwrap();
        let cookie = cookie.to_str().unwrap();
        for part in [
            "__Host-histae_billing=",
            "HttpOnly",
            "SameSite=Strict",
            "Secure",
            "Path=/",
            "Max-Age=1800",
        ] {
            assert!(cookie.contains(part));
        }
        assert!(!cookie.contains("Domain="));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("__Host-histae_billing={secret}").parse().unwrap(),
        );
        assert_eq!(token(&headers, &config), Some(secret));
        headers.append(
            header::COOKIE,
            "__Host-histae_billing=other".parse().unwrap(),
        );
        assert!(token(&headers, &config).is_none());
    }
    #[test]
    fn every_mutation_requires_the_exact_enabled_website_origin() {
        let mut config = config();
        let mut headers = HeaderMap::new();
        assert!(validate_origin(&config, &headers).is_err());
        for value in [
            "null",
            "https://www.histae.com.evil.test",
            "http://www.histae.com",
            "https://dashboard.histae.com",
        ] {
            headers.insert(header::ORIGIN, value.parse().unwrap());
            assert!(validate_origin(&config, &headers).is_err());
        }
        headers.insert(header::ORIGIN, "https://www.histae.com".parse().unwrap());
        assert!(validate_origin(&config, &headers).is_ok());
        headers.append(header::ORIGIN, "https://www.histae.com".parse().unwrap());
        assert!(validate_origin(&config, &headers).is_err());
        headers.remove(header::ORIGIN);
        headers.insert(header::ORIGIN, "https://www.histae.com".parse().unwrap());
        config.enabled = false;
        assert!(validate_origin(&config, &headers).is_err());
        assert!(validate_website_origin(&config, &headers).is_ok());
        headers.insert(header::ORIGIN, "https://evil.test".parse().unwrap());
        assert!(validate_website_origin(&config, &headers).is_err());
    }
}
