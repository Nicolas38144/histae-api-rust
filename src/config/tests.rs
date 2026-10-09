use super::validation::trust_proxy;
use super::*;

#[test]
fn environment_debug_never_exposes_keys_or_values() {
    let secret = uuid::Uuid::new_v4().to_string();
    let source = EnvironmentSource::from_pairs([("PRIVATE_TEST_KEY", secret.as_str())]);
    let debug = format!("{source:?}");
    assert!(debug.contains("entry_count: 1"));
    assert!(!debug.contains(&secret));
    assert!(!debug.contains("PRIVATE_TEST_KEY"));
}

fn base(overrides: &[(&str, &str)]) -> EnvironmentSource {
    let mut values = vec![
        ("ENV", "test"),
        ("JWT_SECRET", "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj"),
        ("PHONE_ENCRYPTION_KEY", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
        ("PHONE_HASH_KEY", "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh"),
        ("POSTGRES_HOST", "localhost"),
        ("POSTGRES_USER", "postgres"),
        ("POSTGRES_PASSWORD", "test-password"),
        ("POSTGRES_DB", "histae-test"),
        ("RATE_LIMIT_STORE", "memory"),
        ("SMS_PROVIDER", "disabled"),
    ];
    values.extend_from_slice(overrides);
    EnvironmentSource::from_pairs(values)
}

#[test]
fn loads_compatible_defaults_without_exposing_secrets() {
    let config = AppConfig::from_source(&base(&[]))
        .unwrap_or_else(|error| panic!("unexpected safe config failure: {error}"));
    assert_eq!(config.port, 8080);
    assert_eq!(
        config.object_storage.endpoint.as_str(),
        "http://storage.histae.localhost:8333/"
    );
    assert_eq!(config.admin_auth.origin, "http://localhost:5173");
    assert!(!config.admin_auth.secure_cookie);
    assert_eq!(config.admin_auth.cookie_name, "histae_admin_session");
    assert_eq!(
        format!("{:?}", config.jwt.secret),
        "SecretString([REDACTED])"
    );
    assert!(!format!("{config:?}").contains("jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj"));
}

#[test]
fn https_admin_origins_use_host_secure_cookies_in_development_and_test() {
    for environment in ["development", "test"] {
        let config = AppConfig::from_source(&base(&[
            ("ENV", environment),
            ("ADMIN_WEBAUTHN_ORIGIN", "https://dashboard.histae.test"),
        ]))
        .expect("valid HTTPS admin configuration");
        assert!(config.admin_auth.secure_cookie);
        assert_eq!(config.admin_auth.cookie_name, "__Host-histae_admin_session");
        let cookie = crate::identity::admin::http::session_cookie("test-token", &config.admin_auth);
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("; HttpOnly; SameSite=Strict"));
        assert!(cookie.contains("; Path=/;"));
        assert!(!cookie.contains("Domain="));
        let expired = crate::identity::admin::http::expired_session_cookie(&config.admin_auth);
        assert!(expired.starts_with("__Host-histae_admin_session="));
        assert!(expired.contains("Max-Age=0"));
        assert!(expired.ends_with("; Secure"));
    }
}

#[test]
fn rejects_invalid_and_conflicting_secrets_without_echoing_values() {
    let source = base(&[("PHONE_HASH_KEY", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")]);
    let error = AppConfig::from_source(&source).expect_err("same key material must fail");
    assert_eq!(error.kind, ConfigErrorKind::Conflict);
    assert!(!error.to_string().contains("eeee"));
    let short = AppConfig::from_source(&base(&[("JWT_SECRET", "private-short-secret")]))
        .expect_err("short jwt secret must fail");
    assert_eq!(short.variable, "JWT_SECRET");
    assert!(!short.to_string().contains("private-short-secret"));
}

#[test]
fn accepts_hex_phone_keys_and_bounded_previous_jwt_keys() {
    let config = AppConfig::from_source(&base(&[
        (
            "PHONE_ENCRYPTION_KEY",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        (
            "JWT_PREVIOUS_KEYS",
            "{\"old\":\"old-signing-secret-that-is-at-least-32-bytes\"}",
        ),
    ]))
    .unwrap_or_else(|error| panic!("unexpected safe config failure: {error}"));
    assert_eq!(config.jwt.verification_keys.len(), 2);
}

#[test]
fn applies_production_transport_and_provider_constraints() {
    let error = AppConfig::from_source(&base(&[("ENV", "production")]))
        .expect_err("incomplete production config must fail");
    assert!(matches!(
        error.kind,
        ConfigErrorKind::Invalid | ConfigErrorKind::Missing
    ));
    assert_eq!(
        trust_proxy("true", Environment::Production),
        Err(ConfigError::invalid("TRUST_PROXY"))
    );
}

#[test]
fn validates_bounds_and_url_origins() {
    assert_eq!(
        AppConfig::from_source(&base(&[("PORT", "0")]))
            .expect_err("zero port")
            .variable,
        "PORT"
    );
    assert_eq!(
        AppConfig::from_source(&base(&[(
            "CORS_ORIGINS",
            "http://localhost:5173,http://localhost:5173"
        )]))
        .expect_err("duplicate origin")
        .variable,
        "CORS_ORIGINS"
    );
    assert_eq!(
        AppConfig::from_source(&base(&[("DATA_EXPORT_MAX_CONCURRENCY", "17")]))
            .expect_err("unbounded concurrency")
            .variable,
        "DATA_EXPORT_MAX_CONCURRENCY"
    );
}

#[test]
fn accepts_the_complete_production_provider_configuration() {
    let source = base(&[
        ("ENV", "production"),
        ("POSTGRES_SSLMODE", "require"),
        ("RATE_LIMIT_STORE", "redis"),
        ("REDIS_ADDR", "redis.internal:6379"),
        ("REDIS_TLS", "true"),
        ("REDIS_PASSWORD", "redis-password"),
        ("OBJECT_STORAGE_ENDPOINT", "https://storage.histae.test"),
        ("OBJECT_STORAGE_ACCESS_KEY", "object-access"),
        ("OBJECT_STORAGE_SECRET_KEY", "object-secret"),
        ("SMS_PROVIDER", "sweego"),
        ("SWEEGO_API_KEY", "sweego-key"),
        ("SWEEGO_SMS_SENDER_ID", "Histae"),
        ("TERMS_OF_SERVICE_VERSION", "v1"),
        ("TERMS_OF_SERVICE_URL", "https://histae.test/terms"),
        ("PRIVACY_POLICY_VERSION", "v1"),
        ("PRIVACY_POLICY_URL", "https://histae.test/privacy"),
        ("SENSITIVE_DATA_CONSENT_VERSION", "v1"),
        (
            "SENSITIVE_DATA_CONSENT_URL",
            "https://histae.test/sensitive",
        ),
        ("LOCATION_CONSENT_VERSION", "v1"),
        ("LOCATION_CONSENT_URL", "https://histae.test/location"),
        ("LEGAL_REVIEW_REFERENCE", "review-2026"),
        ("ADMIN_WEBAUTHN_ORIGIN", "https://admin.histae.test"),
        ("ADMIN_WEBAUTHN_RP_ID", "admin.histae.test"),
        ("BILLING_PROVIDER", "stripe"),
        ("STRIPE_SECRET_KEY", "sk_live_histaeSecret"),
        ("STRIPE_WEBHOOK_SECRET", "whsec_histaeWebhookSecret"),
        ("STRIPE_PREMIUM_PRODUCT_ID", "prod_histaePremium"),
        ("STRIPE_PREMIUM_MONTHLY_PRICE_ID", "price_histaeMonthly"),
        ("STRIPE_PREMIUM_ANNUAL_PRICE_ID", "price_histaeAnnual"),
        (
            "STRIPE_CHECKOUT_SUCCESS_URL",
            "https://app.histae.test/billing/success?session_id={CHECKOUT_SESSION_ID}",
        ),
        (
            "STRIPE_CHECKOUT_CANCEL_URL",
            "https://app.histae.test/billing/cancel",
        ),
        (
            "STRIPE_PORTAL_RETURN_URL",
            "https://app.histae.test/settings/subscription",
        ),
    ]);
    let config = AppConfig::from_source(&source)
        .unwrap_or_else(|error| panic!("unexpected safe production failure: {error}"));
    assert_eq!(config.environment, Environment::Production);
    assert!(config.postgres.tls);
    assert_eq!(config.sms.provider, SmsProvider::Sweego);
    assert_eq!(config.billing.provider, BillingProvider::Stripe);
    assert_eq!(config.rate_limit.store, RateLimitStore::Redis);
    assert_eq!(config.admin_auth.cookie_name, "__Host-histae_admin_session");
}

#[test]
fn enforces_provider_credentials_and_metrics_secret_independence() {
    let fcm = AppConfig::from_source(&base(&[("PUSH_PROVIDER", "fcm")]))
        .expect_err("FCM without a complete service account must fail");
    assert_eq!(fcm.variable, "FIREBASE_SERVICE_ACCOUNT");

    let moderation = AppConfig::from_source(&base(&[
        ("PHOTO_MODERATION_PROVIDER", "local_http"),
        ("PHOTO_MODERATION_TOKEN", "short"),
    ]))
    .expect_err("local moderation requires a strong shared token");
    assert_eq!(moderation.variable, "PHOTO_MODERATION_TOKEN");

    let metrics = AppConfig::from_source(&base(&[
        ("METRICS_ENABLED", "true"),
        ("METRICS_TOKEN", "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj"),
    ]))
    .expect_err("metrics token must be independent");
    assert_eq!(metrics.kind, ConfigErrorKind::Conflict);
}

#[test]
fn accepts_explicit_proxy_networks_and_cors_origins() {
    let config = AppConfig::from_source(&base(&[
        ("TRUST_PROXY", "127.0.0.1,10.0.0.0/8,2001:db8::/32"),
        (
            "CORS_ORIGINS",
            "https://admin.histae.test,http://localhost:5173",
        ),
    ]))
    .unwrap_or_else(|error| panic!("unexpected safe network configuration failure: {error}"));
    assert!(matches!(config.trust_proxy, TrustProxy::Networks(ref entries) if entries.len() == 3));
    assert_eq!(config.cors_origins.len(), 2);
}
