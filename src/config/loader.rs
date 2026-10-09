use super::{ConfigError, EnvironmentSource, types::*, validation::*};
use regex::Regex;
use std::{path::PathBuf, time::Duration};
use url::Url;

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        // Nest's dotenv load is best effort; required variables below still fail closed.
        let _ = dotenvy::dotenv();
        Self::from_source(&EnvironmentSource::from_pairs(std::env::vars_os()))
    }

    pub fn from_source(source: &EnvironmentSource) -> Result<Self, ConfigError> {
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

        let postgres = PostgresConfig {
            host: source.required("POSTGRES_HOST")?,
            port: integer(
                &source.or("POSTGRES_PORT", "5432")?,
                "POSTGRES_PORT",
                1,
                65_535,
            )? as u16,
            user: source.required("POSTGRES_USER")?,
            password: SecretString::new(source.required("POSTGRES_PASSWORD")?),
            database: source.required("POSTGRES_DB")?,
            tls: source.or("POSTGRES_SSLMODE", "disable")? != "disable",
            max_connections: integer(
                &source.or("POSTGRES_POOL_MAX", "20")?,
                "POSTGRES_POOL_MAX",
                1,
                200,
            )? as u32,
            connect_timeout: duration(
                &source.or("POSTGRES_CONNECT_TIMEOUT", "5s")?,
                "POSTGRES_CONNECT_TIMEOUT",
            )?,
            idle_timeout: duration(
                &source.or("POSTGRES_IDLE_TIMEOUT", "30s")?,
                "POSTGRES_IDLE_TIMEOUT",
            )?,
            statement_timeout: duration(
                &source.or("POSTGRES_STATEMENT_TIMEOUT", "15s")?,
                "POSTGRES_STATEMENT_TIMEOUT",
            )?,
            idle_transaction_timeout: duration(
                &source.or("POSTGRES_IDLE_TRANSACTION_TIMEOUT", "30s")?,
                "POSTGRES_IDLE_TRANSACTION_TIMEOUT",
            )?,
            application_name: "histae-api",
            root_certificate: source
                .value("NODE_EXTRA_CA_CERTS")?
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        };
        if environment == Environment::Production && !postgres.tls {
            return Err(ConfigError::invalid("POSTGRES_SSLMODE"));
        }

        let object_access = source.or(
            "OBJECT_STORAGE_ACCESS_KEY",
            if environment == Environment::Production {
                ""
            } else {
                "histae-dev"
            },
        )?;
        let object_secret = source.or(
            "OBJECT_STORAGE_SECRET_KEY",
            if environment == Environment::Production {
                ""
            } else {
                "histae-dev-secret-change-me"
            },
        )?;
        if object_access.is_empty() || object_secret.is_empty() {
            return Err(ConfigError::missing("OBJECT_STORAGE_CREDENTIALS"));
        }
        let object_storage = ObjectStorageConfig {
            endpoint: object_storage_endpoint(
                &source.or(
                    "OBJECT_STORAGE_ENDPOINT",
                    "http://storage.histae.localhost:8333",
                )?,
                environment,
            )?,
            region: storage_region(&source.or("OBJECT_STORAGE_REGION", "us-east-1")?)?,
            bucket: storage_bucket(&source.or("OBJECT_STORAGE_BUCKET", "histae-photos")?)?,
            access_key: object_access,
            secret_key: SecretString::new(object_secret),
            force_path_style: boolean(
                source.value("OBJECT_STORAGE_FORCE_PATH_STYLE")?.as_deref(),
                true,
                "OBJECT_STORAGE_FORCE_PATH_STYLE",
            )?,
        };

        let photo_provider = match source.or("PHOTO_MODERATION_PROVIDER", "disabled")?.as_str() {
            "disabled" => PhotoModerationProvider::Disabled,
            "local_http" => PhotoModerationProvider::LocalHttp,
            _ => return Err(ConfigError::invalid("PHOTO_MODERATION_PROVIDER")),
        };
        let photo_token = source.or("PHOTO_MODERATION_TOKEN", "")?;
        if photo_provider == PhotoModerationProvider::LocalHttp && photo_token.len() < 32 {
            return Err(ConfigError::invalid("PHOTO_MODERATION_TOKEN"));
        }
        let photo_timeout = duration(
            &source.or("PHOTO_MODERATION_TIMEOUT", "5s")?,
            "PHOTO_MODERATION_TIMEOUT",
        )?;
        if photo_timeout > Duration::from_secs(30) {
            return Err(ConfigError::invalid("PHOTO_MODERATION_TIMEOUT"));
        }
        let photo_moderation = PhotoModerationConfig {
            provider: photo_provider,
            endpoint: http_origin(
                &source.or("PHOTO_MODERATION_ENDPOINT", "http://127.0.0.1:8090")?,
                "PHOTO_MODERATION_ENDPOINT",
            )?,
            token: SecretString::new(photo_token),
            timeout: photo_timeout,
            min_sharpness_score: number(
                &source.or("PHOTO_MODERATION_MIN_SHARPNESS", "80")?,
                "PHOTO_MODERATION_MIN_SHARPNESS",
                0.0,
                1_000_000.0,
            )?,
            nsfw_review_threshold: number(
                &source.or("PHOTO_MODERATION_NSFW_REVIEW_THRESHOLD", "0.7")?,
                "PHOTO_MODERATION_NSFW_REVIEW_THRESHOLD",
                0.0,
                1.0,
            )?,
        };

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

        let sms_provider = match source
            .or(
                "SMS_PROVIDER",
                if environment == Environment::Production {
                    "sweego"
                } else {
                    "disabled"
                },
            )?
            .as_str()
        {
            "disabled" => SmsProvider::Disabled,
            "sweego" => SmsProvider::Sweego,
            _ => return Err(ConfigError::invalid("SMS_PROVIDER")),
        };
        if environment == Environment::Production && sms_provider != SmsProvider::Sweego {
            return Err(ConfigError::invalid("SMS_PROVIDER"));
        }
        let sms_api_key = source.or("SWEEGO_API_KEY", "")?;
        let sms_sender = source.or("SWEEGO_SMS_SENDER_ID", "")?;
        if sms_provider == SmsProvider::Sweego && (sms_api_key.is_empty() || sms_sender.is_empty())
        {
            return Err(ConfigError::missing("SWEEGO_CREDENTIALS"));
        }
        if !sms_sender.is_empty()
            && !Regex::new(r"^[A-Za-z0-9]{3,11}$")
                .map_err(|_| ConfigError::invalid("SWEEGO_SMS_SENDER_ID"))?
                .is_match(&sms_sender)
        {
            return Err(ConfigError::invalid("SWEEGO_SMS_SENDER_ID"));
        }
        let sms_region = source.or("SWEEGO_SMS_REGION", "FR")?.to_ascii_uppercase();
        if sms_region != "FR" {
            return Err(ConfigError::invalid("SWEEGO_SMS_REGION"));
        }
        let sms_timeout = duration(&source.or("SWEEGO_TIMEOUT", "10s")?, "SWEEGO_TIMEOUT")?;
        if sms_timeout > Duration::from_secs(30) {
            return Err(ConfigError::invalid("SWEEGO_TIMEOUT"));
        }
        let otp_ttl = duration(&source.or("OTP_TTL", "10m")?, "OTP_TTL")?;
        ensure_duration(
            otp_ttl,
            Duration::from_secs(60),
            Duration::from_secs(1_800),
            "OTP_TTL",
        )?;
        let webhook_secret = source.or("SWEEGO_WEBHOOK_SECRET", "")?;
        if !webhook_secret.is_empty()
            && (webhook_secret.len() != 64
                || !webhook_secret
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'))
        {
            return Err(ConfigError::invalid("SWEEGO_WEBHOOK_SECRET"));
        }
        let sms = SmsConfig {
            provider: sms_provider,
            endpoint: https_url(
                &source.or("SWEEGO_API_URL", "https://api.sweego.io/send")?,
                "SWEEGO_API_URL",
            )?,
            api_key: SecretString::new(sms_api_key),
            sender_id: sms_sender,
            region: sms_region,
            timeout: sms_timeout,
            otp_ttl,
            webhook_secret: SecretString::new(webhook_secret),
        };

        let legal = legal_config(source, environment)?;
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
        let maintenance_mode = match source
            .or(
                "MAINTENANCE_MODE",
                if environment == Environment::Production {
                    "disabled"
                } else {
                    "api"
                },
            )?
            .as_str()
        {
            "api" => MaintenanceMode::Api,
            "worker" => MaintenanceMode::Worker,
            "disabled" => MaintenanceMode::Disabled,
            _ => return Err(ConfigError::invalid("MAINTENANCE_MODE")),
        };
        let workloads = WorkloadConfig {
            match_maintenance_batch_size: integer(
                &source.or("MATCH_MAINTENANCE_BATCH_SIZE", "500")?,
                "MATCH_MAINTENANCE_BATCH_SIZE",
                1,
                5_000,
            )? as u32,
            match_maintenance_max_batches: integer(
                &source.or("MATCH_MAINTENANCE_MAX_BATCHES", "20")?,
                "MATCH_MAINTENANCE_MAX_BATCHES",
                1,
                1_000,
            )? as u32,
            outbox_purge_batch_size: integer(
                &source.or("OUTBOX_PURGE_BATCH_SIZE", "500")?,
                "OUTBOX_PURGE_BATCH_SIZE",
                1,
                5_000,
            )? as u32,
            outbox_purge_max_batches: integer(
                &source.or("OUTBOX_PURGE_MAX_BATCHES", "20")?,
                "OUTBOX_PURGE_MAX_BATCHES",
                1,
                1_000,
            )? as u32,
            data_export_page_size: integer(
                &source.or("DATA_EXPORT_PAGE_SIZE", "250")?,
                "DATA_EXPORT_PAGE_SIZE",
                10,
                2_000,
            )? as u32,
            data_export_max_bytes: integer(
                &source.or("DATA_EXPORT_MAX_BYTES", "536870912")?,
                "DATA_EXPORT_MAX_BYTES",
                1_048_576,
                2_147_483_647,
            )?,
            data_export_max_concurrency: integer(
                &source.or("DATA_EXPORT_MAX_CONCURRENCY", "2")?,
                "DATA_EXPORT_MAX_CONCURRENCY",
                1,
                16,
            )? as u8,
        };

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
            "PHOTO_MODERATION_TOKEN",
            "SWEEGO_API_KEY",
            "SWEEGO_WEBHOOK_SECRET",
            "FIREBASE_PRIVATE_KEY",
            "STRIPE_SECRET_KEY",
            "STRIPE_WEBHOOK_SECRET",
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
        let configured_redis_address = source.or("REDIS_ADDR", "")?;
        if rate_store == RateLimitStore::Redis && configured_redis_address.is_empty() {
            return Err(ConfigError::missing("REDIS_ADDR"));
        }
        let redis_address = if configured_redis_address.is_empty() {
            "localhost:6379".to_owned()
        } else {
            configured_redis_address
        };
        let redis_regex = Regex::new(r"^[a-zA-Z0-9._-]+:\d{1,5}$")
            .map_err(|_| ConfigError::invalid("REDIS_ADDR"))?;
        if !redis_regex.is_match(&redis_address) {
            return Err(ConfigError::invalid("REDIS_ADDR"));
        }
        let redis_password = source.raw_value("REDIS_PASSWORD")?.unwrap_or_default();
        let redis_tls = boolean(source.value("REDIS_TLS")?.as_deref(), false, "REDIS_TLS")?;
        if environment == Environment::Production && (!redis_tls || redis_password.is_empty()) {
            return Err(ConfigError::invalid("REDIS_SECURITY"));
        }
        let redis = RedisConfig {
            address: redis_address,
            password: SecretString::new(redis_password),
            db: integer(&source.or("REDIS_DB", "0")?, "REDIS_DB", 0, 15)? as u8,
            tls: redis_tls,
            connect_timeout: duration(
                &source.or("REDIS_CONNECT_TIMEOUT", "5s")?,
                "REDIS_CONNECT_TIMEOUT",
            )?,
            command_timeout: duration(
                &source.or("REDIS_COMMAND_TIMEOUT", "1s")?,
                "REDIS_COMMAND_TIMEOUT",
            )?,
            root_certificate: source
                .value("NODE_EXTRA_CA_CERTS")?
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
        };

        let push_provider = match source
            .or("PUSH_PROVIDER", "disabled")?
            .to_ascii_lowercase()
            .as_str()
        {
            "disabled" => PushProvider::Disabled,
            "fcm" => PushProvider::Fcm,
            _ => return Err(ConfigError::invalid("PUSH_PROVIDER")),
        };
        let project_id = source.or("FIREBASE_PROJECT_ID", "")?;
        let client_email = source.or("FIREBASE_CLIENT_EMAIL", "")?;
        let private_key = source
            .value("FIREBASE_PRIVATE_KEY")?
            .unwrap_or_default()
            .replace("\\n", "\n")
            .trim()
            .to_owned();
        if push_provider == PushProvider::Fcm
            && (project_id.is_empty() || client_email.is_empty() || private_key.is_empty())
        {
            return Err(ConfigError::missing("FIREBASE_SERVICE_ACCOUNT"));
        }
        let push_timeout = duration(&source.or("PUSH_TIMEOUT", "5s")?, "PUSH_TIMEOUT")?;
        if push_timeout > Duration::from_secs(30) {
            return Err(ConfigError::invalid("PUSH_TIMEOUT"));
        }
        let push = PushConfig {
            provider: push_provider,
            project_id,
            client_email,
            private_key: SecretString::new(private_key),
            token_uri: https_url(
                &source.or("FIREBASE_TOKEN_URI", "https://oauth2.googleapis.com/token")?,
                "FIREBASE_TOKEN_URI",
            )?,
            timeout: push_timeout,
        };

        let billing = billing_config(source, environment)?;
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

        Ok(Self {
            environment,
            port,
            postgres,
            jwt,
            account_deletion_token_ttl,
            phone,
            sms,
            redis,
            push,
            billing,
            object_storage,
            photo_moderation,
            admin_auth,
            legal,
            trust_proxy,
            cors_origins,
            maintenance_mode,
            workloads,
            metrics,
            rate_limit,
        })
    }
}
