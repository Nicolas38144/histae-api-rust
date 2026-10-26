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
fn sms_budget_defaults_and_emergency_switch_are_bounded() {
    let config = AppConfig::from_source(&base(&[])).expect("defaults");
    assert_eq!(config.sms.safety.hourly_per_phone, 5);
    assert_eq!(config.sms.safety.daily_per_phone, 10);
    assert_eq!(config.sms.safety.hourly_global, 100);
    assert_eq!(config.sms.safety.daily_global, 500);
    assert_eq!(
        config.sms.resend_cooldown,
        std::time::Duration::from_secs(60)
    );
    assert!(!config.sms.safety.suspended);
    assert!(
        AppConfig::from_source(&base(&[("SMS_SENDING_SUSPENDED", "true")]))
            .expect("switch")
            .sms
            .safety
            .suspended
    );
    for (key, value) in [
        ("SMS_MAX_PER_PHONE_HOUR", "0"),
        ("SMS_MAX_PER_PHONE_DAY", "4"),
        ("SMS_MAX_GLOBAL_HOUR", "0"),
        ("SMS_MAX_GLOBAL_DAY", "99"),
        ("OTP_RESEND_COOLDOWN", "59s"),
        ("SMS_ALERT_SPIKE_5M", "0"),
        ("SMS_SENDING_SUSPENDED", "yes"),
    ] {
        assert_eq!(
            AppConfig::from_source(&base(&[(key, value)]))
                .expect_err("invalid policy")
                .variable,
            key
        );
    }
}

#[test]
fn resend_cooldown_cannot_outlive_the_code() {
    assert_eq!(
        AppConfig::from_source(&base(&[("OTP_TTL", "60s"), ("OTP_RESEND_COOLDOWN", "61s")]))
            .expect_err("cooldown longer than code lifetime")
            .variable,
        "OTP_RESEND_COOLDOWN"
    );
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
        #[cfg(feature = "webauthn-probe")]
        {
            let cookie =
                crate::identity::admin::http::session_cookie("test-token", &config.admin_auth);
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
    assert_eq!(
        config.billing.premium_monthly_price_id,
        "price_histaeMonthly"
    );
    assert_eq!(config.billing.premium_annual_price_id, "price_histaeAnnual");
    assert_eq!(config.rate_limit.store, RateLimitStore::Redis);
    assert_eq!(config.admin_auth.cookie_name, "__Host-histae_admin_session");
}

#[test]
fn stripe_requires_two_distinct_price_ids() {
    let billing = [
        ("BILLING_PROVIDER", "stripe"),
        ("STRIPE_SECRET_KEY", "sk_test_histaeSecret"),
        ("STRIPE_WEBHOOK_SECRET", "whsec_histaeWebhookSecret"),
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
    ];
    for invalid_annual_id in ["", "sub_histaeAnnual", "price_histaeMonthly"] {
        let mut values = billing.to_vec();
        values.push(("STRIPE_PREMIUM_ANNUAL_PRICE_ID", invalid_annual_id));
        let error = AppConfig::from_source(&base(&values))
            .expect_err("Stripe requires a distinct recurring price for each billing period");
        assert_eq!(error.variable, "STRIPE_PRICE_IDS");
    }
}

#[test]
fn enforces_provider_credentials_and_metrics_secret_independence() {
    let fcm = AppConfig::from_source(&base(&[("PUSH_PROVIDER", "fcm")]))
        .expect_err("FCM without a complete service account must fail");
    assert_eq!(fcm.variable, "FIREBASE_SERVICE_ACCOUNT");

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

#[test]
fn advertising_defaults_to_disabled_and_requires_well_formed_ad_units() {
    assert!(
        !AppConfig::from_source(&base(&[]))
            .unwrap()
            .advertising
            .enabled
    );
    assert!(AppConfig::from_source(&base(&[("ADVERTISING_PROVIDER", "admob")])).is_err());
    let config = AppConfig::from_source(&base(&[
        ("ADVERTISING_PROVIDER", "admob"),
        (
            "ADMOB_ANDROID_REWARDED_AD_UNIT",
            "ca-app-pub-3940256099942544/5224354917",
        ),
        (
            "ADMOB_IOS_REWARDED_AD_UNIT",
            "ca-app-pub-3940256099942544/1712485313",
        ),
    ]))
    .unwrap();
    assert!(config.advertising.enabled);
    assert_eq!(config.advertising.reward_item, "continuation_credit");
}

#[test]
fn translated_legal_urls_are_validated_and_report_the_actual_document_language() {
    use crate::{localization::Language, profiles::domain::ConsentType};
    let config = AppConfig::from_source(&base(&[(
        "TERMS_OF_SERVICE_URL_EN",
        "https://legal.example.org/en/terms",
    )]))
    .expect("configuration");
    assert_eq!(
        config.legal_translations.resolve(
            ConsentType::TermsOfServiceAcceptance,
            "https://legal.example.org/fr/terms",
            Language::En
        ),
        ("https://legal.example.org/en/terms".into(), Language::En)
    );
    assert_eq!(
        config
            .legal_translations
            .resolve(
                ConsentType::TermsOfServiceAcceptance,
                "https://legal.example.org/fr/terms",
                Language::It
            )
            .1,
        Language::Fr
    );
    for url in [
        "http://legal.example.org/en/terms",
        "https://user:password@legal.example.org/en/terms",
        "file:///tmp/terms",
    ] {
        let error = AppConfig::from_source(&base(&[("TERMS_OF_SERVICE_URL_EN", url)]))
            .expect_err("unsafe URL");
        assert_eq!(error.variable, "TERMS_OF_SERVICE_URL_EN");
        assert!(!error.to_string().contains(url));
    }
}
