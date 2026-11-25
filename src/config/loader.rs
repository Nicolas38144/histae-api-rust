use super::{ConfigError, EnvironmentSource, types::*, validation::*};
use regex::Regex;
use std::{path::PathBuf, time::Duration};
use url::Url;

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&EnvironmentSource::runtime())
    }

    pub fn from_api_env() -> Result<Self, ConfigError> {
        Self::load(&EnvironmentSource::runtime(), false)
    }

    pub fn from_source(source: &EnvironmentSource) -> Result<Self, ConfigError> {
        Self::load(source, true)
    }

    fn load(source: &EnvironmentSource, delivery: bool) -> Result<Self, ConfigError> {
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        let port = integer(&source.or("PORT", "8080")?, "PORT", 1, 65_535)? as u16;

        let admin_origin = webauthn_origin(
            &source.or(
                "ADMIN_WEBAUTHN_ORIGIN",
                if environment == Environment::Production {
                    ""
                } else {
                    "http://localhost:5173"
                },
            )?,
            environment,
        )?;
        let origin_url =
            Url::parse(&admin_origin).map_err(|_| ConfigError::invalid("ADMIN_WEBAUTHN_ORIGIN"))?;
        let origin_host = origin_url
            .host_str()
            .ok_or_else(|| ConfigError::invalid("ADMIN_WEBAUTHN_ORIGIN"))?;
        let rp_id = webauthn_rp_id(
            &source.or("ADMIN_WEBAUTHN_RP_ID", origin_host)?,
            origin_host,
            environment,
        )?;
        let rp_name = source.or("ADMIN_WEBAUTHN_RP_NAME", "Histae Administration")?;
        if rp_name.is_empty()
            || rp_name.chars().count() > 64
            || rp_name
                .chars()
                .any(|value| value <= '\u{1f}' || value == '\u{7f}')
        {
            return Err(ConfigError::invalid("ADMIN_WEBAUTHN_RP_NAME"));
        }
        let challenge_ttl = duration(
            &source.or("ADMIN_WEBAUTHN_CHALLENGE_TTL", "5m")?,
            "ADMIN_WEBAUTHN_CHALLENGE_TTL",
        )?;
        ensure_duration(
            challenge_ttl,
            Duration::from_secs(60),
            Duration::from_secs(600),
            "ADMIN_WEBAUTHN_CHALLENGE_TTL",
        )?;
        let bootstrap_ttl = duration(
            &source.or("ADMIN_WEBAUTHN_BOOTSTRAP_TTL", "15m")?,
            "ADMIN_WEBAUTHN_BOOTSTRAP_TTL",
        )?;
        ensure_duration(
            bootstrap_ttl,
            Duration::from_secs(300),
            Duration::from_secs(3_600),
            "ADMIN_WEBAUTHN_BOOTSTRAP_TTL",
        )?;
        let session_idle_ttl = duration(
            &source.or("ADMIN_SESSION_IDLE_TTL", "30m")?,
            "ADMIN_SESSION_IDLE_TTL",
        )?;
        let session_absolute_ttl = duration(
            &source.or("ADMIN_SESSION_ABSOLUTE_TTL", "8h")?,
            "ADMIN_SESSION_ABSOLUTE_TTL",
        )?;
        if session_idle_ttl < Duration::from_secs(300)
            || session_idle_ttl > Duration::from_secs(7_200)
            || session_absolute_ttl < session_idle_ttl
            || session_absolute_ttl > Duration::from_secs(86_400)
        {
            return Err(ConfigError::invalid("ADMIN_SESSION_TTLS"));
        }
        let recent_authentication_ttl = duration(
            &source.or("ADMIN_RECENT_AUTH_TTL", "10m")?,
            "ADMIN_RECENT_AUTH_TTL",
        )?;
        if recent_authentication_ttl > session_idle_ttl {
            return Err(ConfigError::invalid("ADMIN_RECENT_AUTH_TTL"));
        }
        let secure_cookie = origin_url.scheme() == "https";
        let admin_auth = AdminAuthConfig {
            rp_id,
            origin: admin_origin,
            rp_name,
            challenge_ttl,
            bootstrap_ttl,
            session_idle_ttl,
            session_absolute_ttl,
            recent_authentication_ttl,
            cookie_name: if secure_cookie {
                "__Host-histae_admin_session"
            } else {
                "histae_admin_session"
            },
            secure_cookie,
        };

        let jwt_raw = source.required("JWT_SECRET")?;
        if jwt_raw.len() < 32 {
            return Err(ConfigError::invalid("JWT_SECRET"));
        }
        let encryption_raw = source.required("PHONE_ENCRYPTION_KEY")?;
        let hash_raw = source.required("PHONE_HASH_KEY")?;
        let encryption_bytes = phone_key_bytes(&encryption_raw, "PHONE_ENCRYPTION_KEY")?;
        let hash_bytes = phone_key_bytes(&hash_raw, "PHONE_HASH_KEY")?;
        if encryption_bytes == hash_bytes
            || encryption_bytes.as_slice() == jwt_raw.as_bytes()
            || hash_bytes.as_slice() == jwt_raw.as_bytes()
        {
            return Err(ConfigError::conflict("JWT_PHONE_KEYS"));
        }

        let postgres = super::database::load(source, environment)?;

        let object_storage = ObjectStorageConfig::from_source(source, environment)?;

        let access_ttl = duration(&source.or("JWT_ACCESS_TTL", "15m")?, "JWT_ACCESS_TTL")?;
        ensure_duration(
            access_ttl,
            Duration::from_secs(60),
            Duration::from_secs(3_600),
            "JWT_ACCESS_TTL",
        )?;
        let refresh_ttl = duration(&source.or("JWT_REFRESH_TTL", "4320h")?, "JWT_REFRESH_TTL")?;
        if refresh_ttl < Duration::from_secs(3_600)
            || refresh_ttl > Duration::from_secs(4_320 * 3_600)
            || refresh_ttl <= access_ttl
        {
            return Err(ConfigError::invalid("JWT_REFRESH_TTL"));
        }
        let active_kid = source.or("JWT_ACTIVE_KID", "primary")?;
        let verification_keys = jwt_keys(
            &active_kid,
            &jwt_raw,
            &source.or("JWT_PREVIOUS_KEYS", "{}")?,
        )?;
        if verification_keys
            .values()
            .any(|key| key.as_bytes() == encryption_bytes || key.as_bytes() == hash_bytes)
        {
            return Err(ConfigError::conflict("JWT_PHONE_KEYS"));
        }
        let jwt = JwtConfig {
            secret: SecretString::new(jwt_raw),
            active_kid,
            verification_keys,
            access_ttl,
            refresh_ttl,
        };
        let account_deletion_token_ttl = duration(
            &source.or("ACCOUNT_DELETION_TOKEN_TTL", "10m")?,
            "ACCOUNT_DELETION_TOKEN_TTL",
        )?;
        ensure_duration(
            account_deletion_token_ttl,
            Duration::from_secs(60),
            Duration::from_secs(1_800),
            "ACCOUNT_DELETION_TOKEN_TTL",
        )?;
        let phone = PhoneConfig {
            encryption_key: SecretString::new(encryption_raw),
            hash_key: SecretString::new(hash_raw),
        };

        let sms = super::runtime::sms(source, environment, delivery, true)?;
        let legal = legal_config(source, environment)?;
        let legal_translations = legal_translations_config(source, environment)?;
        let trust_proxy = trust_proxy(&source.or("TRUST_PROXY", "false")?, environment)?;
        let cors_origins = web_origins(
            &source.or(
                "CORS_ORIGINS",
                if environment == Environment::Development {
                    "http://localhost:5173"
                } else {
                    ""
                },
            )?,
            environment,
        )?;
        let maintenance_mode = super::runtime::maintenance_mode(source, environment)?;
        let workloads = super::runtime::workloads(source)?;

        let metrics_enabled = boolean(
            source.value("METRICS_ENABLED")?.as_deref(),
            false,
            "METRICS_ENABLED",
        )?;
        let metrics_host = source.or("METRICS_HOST", "127.0.0.1")?;
        let host_regex =
            Regex::new(r"^(?:localhost|[A-Za-z0-9](?:[A-Za-z0-9.:-]{0,251}[A-Za-z0-9])?)$")
                .map_err(|_| ConfigError::invalid("METRICS_HOST"))?;
        if !host_regex.is_match(&metrics_host) {
            return Err(ConfigError::invalid("METRICS_HOST"));
        }
        let metrics_token = source.or("METRICS_TOKEN", "")?;
        if metrics_enabled && metrics_token.len() < 32 {
            return Err(ConfigError::invalid("METRICS_TOKEN"));
        }
        let other_secret_names = [
            "JWT_SECRET",
            "PHONE_ENCRYPTION_KEY",
            "PHONE_HASH_KEY",
            "POSTGRES_PASSWORD",
            "REDIS_PASSWORD",
            "OBJECT_STORAGE_SECRET_KEY",
            "SWEEGO_API_KEY",
            "SWEEGO_WEBHOOK_SECRET",
            "FIREBASE_PRIVATE_KEY",
            "GRAFANA_ADMIN_PASSWORD",
        ];
        if metrics_enabled
            && other_secret_names.iter().any(|name| {
                source
                    .raw_value(name)
                    .ok()
                    .flatten()
                    .is_some_and(|secret| !secret.is_empty() && secret == metrics_token)
            })
        {
            return Err(ConfigError::conflict("METRICS_TOKEN"));
        }
        let metrics = MetricsConfig {
            enabled: metrics_enabled,
            host: metrics_host,
            port: integer(
                &source.or("METRICS_PORT", "9091")?,
                "METRICS_PORT",
                1,
                65_535,
            )? as u16,
            token: SecretString::new(metrics_token),
        };

        let rate_store = match source
            .or("RATE_LIMIT_STORE", "memory")?
            .to_ascii_lowercase()
            .as_str()
        {
            "memory" => RateLimitStore::Memory,
            "redis" => RateLimitStore::Redis,
            _ => return Err(ConfigError::invalid("RATE_LIMIT_STORE")),
        };
        if environment == Environment::Production && rate_store != RateLimitStore::Redis {
            return Err(ConfigError::invalid("RATE_LIMIT_STORE"));
        }
        let redis =
            super::runtime::redis(source, environment, rate_store == RateLimitStore::Redis)?;
        let push = if delivery {
            super::runtime::push(source)?
        } else {
            super::runtime::push(&EnvironmentSource::default())?
        };

        let advertising_provider = source.or("ADVERTISING_PROVIDER", "disabled")?;
        let advertising_enabled = match advertising_provider.as_str() {
            "disabled" => false,
            "admob" => true,
            _ => return Err(ConfigError::invalid("ADVERTISING_PROVIDER")),
        };
        let android_ad_unit = source.or("ADMOB_ANDROID_REWARDED_AD_UNIT", "")?;
        let ios_ad_unit = source.or("ADMOB_IOS_REWARDED_AD_UNIT", "")?;
        let reward_item = source.or("ADMOB_REWARD_ITEM", "continuation_credit")?;
        if advertising_enabled {
            let format = Regex::new(r"^ca-app-pub-[0-9]{16}/[0-9]{10}$")
                .map_err(|_| ConfigError::invalid("ADMOB_AD_UNITS"))?;
            if !format.is_match(&android_ad_unit)
                || !format.is_match(&ios_ad_unit)
                || (environment == Environment::Production
                    && [&android_ad_unit, &ios_ad_unit]
                        .iter()
                        .any(|unit| unit.contains("3940256099942544")))
            {
                return Err(ConfigError::invalid("ADMOB_AD_UNITS"));
            }
            if reward_item.is_empty()
                || reward_item.len() > 64
                || !reward_item
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(ConfigError::invalid("ADMOB_REWARD_ITEM"));
            }
        }
        let advertising = AdvertisingConfig {
            enabled: advertising_enabled,
            android_ad_unit,
            ios_ad_unit,
            reward_item,
        };
        let rate_limit = RateLimitConfig {
            store: rate_store,
            global: limit(source, "RATE_LIMIT_GLOBAL", 100, "1m")?,
            otp: limit(source, "RATE_LIMIT_OTP", 5, "1h")?,
            refresh: limit(source, "RATE_LIMIT_REFRESH", 30, "15m")?,
            feed: limit(source, "RATE_LIMIT_FEED", 60, "1m")?,
            message: limit(source, "RATE_LIMIT_MESSAGE", 60, "1m")?,
            data_export: limit(source, "RATE_LIMIT_DATA_EXPORT", 5, "1h")?,
            report: limit(source, "RATE_LIMIT_REPORT", 5, "1h")?,
            photo: limit(source, "RATE_LIMIT_PHOTO", 10, "1h")?,
            swipe: limit(source, "RATE_LIMIT_SWIPE", 120, "1m")?,
            billing: limit(source, "RATE_LIMIT_BILLING", 10, "1m")?,
            billing_webhook: limit(source, "RATE_LIMIT_BILLING_WEBHOOK", 300, "1m")?,
            sms_webhook: limit(source, "RATE_LIMIT_SMS_WEBHOOK", 300, "1m")?,
            admin_auth: limit(source, "RATE_LIMIT_ADMIN_AUTH", 10, "5m")?,
        };

        let store_billing = super::stores::load(source, environment)?;
        let web_account = super::web_account::load(source, environment)?;
        Ok(Self {
            environment,
            photo_codec_socket: source
                .value("PHOTO_CODEC_SOCKET")?
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            port,
            postgres,
            jwt,
            account_deletion_token_ttl,
            play_review: super::play_review::load(source, &phone.hash_key)?,
            phone,
            mobile_otp_require_challenge: boolean(
                source.value("MOBILE_OTP_REQUIRE_CHALLENGE")?.as_deref(),
                false,
                "MOBILE_OTP_REQUIRE_CHALLENGE",
            )? || environment == Environment::Production,
            sms,
            redis,
            push,
            store_billing,
            web_account,
            advertising,
            object_storage,
            admin_auth,
            legal,
            legal_translations,
            trust_proxy,
            cors_origins,
            maintenance_mode,
            workloads,
            metrics,
            rate_limit,
        })
    }
}
