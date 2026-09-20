use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use url::Url;

#[derive(Clone, Eq, PartialEq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceMode {
    Api,
    Worker,
    Disabled,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsProvider {
    Disabled,
    Sweego,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushProvider {
    Disabled,
    Fcm,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingProvider {
    Disabled,
    Stripe,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhotoModerationProvider {
    Disabled,
    LocalHttp,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateLimitStore {
    Memory,
    Redis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustProxy {
    Disabled,
    All,
    Networks(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: SecretString,
    pub database: String,
    pub tls: bool,
    pub max_connections: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub statement_timeout: Duration,
    pub idle_transaction_timeout: Duration,
    pub application_name: &'static str,
    pub root_certificate: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct JwtConfig {
    pub secret: SecretString,
    pub active_kid: String,
    pub verification_keys: BTreeMap<String, SecretString>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
}

#[derive(Clone, Debug)]
pub struct PhoneConfig {
    pub encryption_key: SecretString,
    pub hash_key: SecretString,
}

#[derive(Clone, Debug)]
pub struct SmsConfig {
    pub provider: SmsProvider,
    pub endpoint: Url,
    pub api_key: SecretString,
    pub sender_id: String,
    pub region: String,
    pub timeout: Duration,
    pub otp_ttl: Duration,
    pub webhook_secret: SecretString,
}

#[derive(Clone, Debug)]
pub struct RedisConfig {
    pub address: String,
    pub password: SecretString,
    pub db: u8,
    pub tls: bool,
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct PushConfig {
    pub provider: PushProvider,
    pub project_id: String,
    pub client_email: String,
    pub private_key: SecretString,
    pub token_uri: Url,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct BillingConfig {
    pub provider: BillingProvider,
    pub stripe_secret_key: SecretString,
    pub stripe_webhook_secret: SecretString,
    pub premium_product_id: String,
    pub premium_monthly_price_id: String,
    pub premium_annual_price_id: String,
    pub checkout_success_url: Option<String>,
    pub checkout_cancel_url: Option<String>,
    pub portal_return_url: Option<String>,
    pub automatic_tax: bool,
    pub allow_promotion_codes: bool,
    pub timeout: Duration,
    pub max_network_retries: u8,
    pub reconciliation_interval: Duration,
    pub reconciliation_freshness: Duration,
    pub reconciliation_batch_size: u16,
}

#[derive(Clone, Debug)]
pub struct ObjectStorageConfig {
    pub endpoint: Url,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: SecretString,
    pub force_path_style: bool,
}

#[derive(Clone, Debug)]
pub struct PhotoModerationConfig {
    pub provider: PhotoModerationProvider,
    pub endpoint: Url,
    pub token: SecretString,
    pub timeout: Duration,
    pub min_sharpness_score: f64,
    pub nsfw_review_threshold: f64,
}

#[derive(Clone, Debug)]
pub struct AdminAuthConfig {
    pub rp_id: String,
    pub origin: String,
    pub rp_name: String,
    pub challenge_ttl: Duration,
    pub bootstrap_ttl: Duration,
    pub session_idle_ttl: Duration,
    pub session_absolute_ttl: Duration,
    pub recent_authentication_ttl: Duration,
    pub cookie_name: &'static str,
    pub secure_cookie: bool,
}

#[derive(Clone, Debug)]
pub struct LegalConfig {
    pub terms_version: String,
    pub privacy_version: String,
    pub sensitive_data_consent_version: String,
    pub location_consent_version: String,
    pub terms_url: Url,
    pub privacy_url: Url,
    pub sensitive_data_consent_url: Url,
    pub location_consent_url: Url,
    pub review_reference: String,
}

#[derive(Clone, Debug)]
pub struct WorkloadConfig {
    pub match_maintenance_batch_size: u32,
    pub match_maintenance_max_batches: u32,
    pub outbox_purge_batch_size: u32,
    pub outbox_purge_max_batches: u32,
    pub data_export_page_size: u32,
    pub data_export_max_bytes: u64,
    pub data_export_max_concurrency: u8,
}

#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub token: SecretString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LimitPolicy {
    pub max: u64,
    pub window: Duration,
}

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub store: RateLimitStore,
    pub global: LimitPolicy,
    pub otp: LimitPolicy,
    pub refresh: LimitPolicy,
    pub feed: LimitPolicy,
    pub message: LimitPolicy,
    pub data_export: LimitPolicy,
    pub report: LimitPolicy,
    pub photo: LimitPolicy,
    pub swipe: LimitPolicy,
    pub billing: LimitPolicy,
    pub billing_webhook: LimitPolicy,
    pub sms_webhook: LimitPolicy,
    pub admin_auth: LimitPolicy,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub environment: Environment,
    pub port: u16,
    pub postgres: PostgresConfig,
    pub jwt: JwtConfig,
    pub account_deletion_token_ttl: Duration,
    pub phone: PhoneConfig,
    pub sms: SmsConfig,
    pub redis: RedisConfig,
    pub push: PushConfig,
    pub billing: BillingConfig,
    pub object_storage: ObjectStorageConfig,
    pub photo_moderation: PhotoModerationConfig,
    pub admin_auth: AdminAuthConfig,
    pub legal: LegalConfig,
    pub trust_proxy: TrustProxy,
    pub cors_origins: Vec<String>,
    pub maintenance_mode: MaintenanceMode,
    pub workloads: WorkloadConfig,
    pub metrics: MetricsConfig,
    pub rate_limit: RateLimitConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigErrorKind {
    Missing,
    Invalid,
    Conflict,
    Dotenv,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigError {
    pub variable: &'static str,
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    fn missing(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Missing,
        }
    }
    fn invalid(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Invalid,
        }
    }
    fn conflict(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Conflict,
        }
    }
    pub fn safe_code(self) -> &'static str {
        match self.kind {
            ConfigErrorKind::Missing => "config_missing",
            ConfigErrorKind::Invalid => "config_invalid",
            ConfigErrorKind::Conflict => "config_conflict",
            ConfigErrorKind::Dotenv => "dotenv_failed",
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration error ({})", self.variable)
    }
}
impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, Default)]
pub struct EnvironmentSource(BTreeMap<OsString, OsString>);

impl EnvironmentSource {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    fn value(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        let Some(value) = self.0.get(OsStr::new(name)) else {
            return Ok(None);
        };
        let value = value.to_str().ok_or_else(|| ConfigError::invalid(name))?;
        Ok(Some(value.trim().to_owned()))
    }

    fn raw_value(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        let Some(value) = self.0.get(OsStr::new(name)) else {
            return Ok(None);
        };
        Ok(Some(
            value
                .to_str()
                .ok_or_else(|| ConfigError::invalid(name))?
                .to_owned(),
        ))
    }

    fn or(&self, name: &'static str, fallback: &str) -> Result<String, ConfigError> {
        Ok(self
            .value(name)?
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_owned()))
    }

    fn required(&self, name: &'static str) -> Result<String, ConfigError> {
        self.value(name)?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ConfigError::missing(name))
    }
}

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
        let admin_auth = AdminAuthConfig {
            rp_id,
            origin: admin_origin,
            rp_name,
            challenge_ttl,
            bootstrap_ttl,
            session_idle_ttl,
            session_absolute_ttl,
            recent_authentication_ttl,
            cookie_name: if environment == Environment::Production {
                "__Host-histae_admin_session"
            } else {
                "histae_admin_session"
            },
            secure_cookie: environment == Environment::Production,
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

fn parse_environment(value: Option<&str>) -> Result<Environment, ConfigError> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("development") => Ok(Environment::Development),
        Some("test") => Ok(Environment::Test),
        Some("production") => Ok(Environment::Production),
        _ => Err(ConfigError::invalid("ENV")),
    }
}

fn integer(value: &str, name: &'static str, min: u64, max: u64) -> Result<u64, ConfigError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::invalid(name));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= min && *value <= max)
        .ok_or_else(|| ConfigError::invalid(name))
}

fn duration(value: &str, name: &'static str) -> Result<Duration, ConfigError> {
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

fn ensure_duration(
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

fn boolean(value: Option<&str>, fallback: bool, name: &'static str) -> Result<bool, ConfigError> {
    match value.filter(|value| !value.is_empty()) {
        None => Ok(fallback),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(ConfigError::invalid(name)),
    }
}

fn phone_key_bytes(value: &str, name: &'static str) -> Result<Vec<u8>, ConfigError> {
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

fn jwt_keys(
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

fn number(value: &str, name: &'static str, min: f64, max: f64) -> Result<f64, ConfigError> {
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

fn http_origin(value: &str, name: &'static str) -> Result<Url, ConfigError> {
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

fn object_storage_endpoint(value: &str, env: Environment) -> Result<Url, ConfigError> {
    let url = http_origin(value, "OBJECT_STORAGE_ENDPOINT")?;
    if env == Environment::Production && url.scheme() != "https" {
        Err(ConfigError::invalid("OBJECT_STORAGE_ENDPOINT"))
    } else {
        Ok(url)
    }
}

fn exact_origin(value: &str, name: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.origin().unicode_serialization() != value {
        return Err(ConfigError::invalid(name));
    }
    Ok(url)
}

fn webauthn_origin(value: &str, env: Environment) -> Result<String, ConfigError> {
    let url = exact_origin(value, "ADMIN_WEBAUTHN_ORIGIN")?;
    let local = env != Environment::Production
        && url.scheme() == "http"
        && url.host_str() == Some("localhost");
    if url.scheme() != "https" && !local {
        return Err(ConfigError::invalid("ADMIN_WEBAUTHN_ORIGIN"));
    }
    Ok(url.origin().unicode_serialization())
}

fn webauthn_rp_id(value: &str, host: &str, env: Environment) -> Result<String, ConfigError> {
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

fn https_url(value: &str, name: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.scheme() == "https" {
        Ok(url)
    } else {
        Err(ConfigError::invalid(name))
    }
}

fn web_origins(value: &str, env: Environment) -> Result<Vec<String>, ConfigError> {
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

fn trust_proxy(value: &str, env: Environment) -> Result<TrustProxy, ConfigError> {
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

fn storage_region(value: &str) -> Result<String, ConfigError> {
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

fn storage_bucket(value: &str) -> Result<String, ConfigError> {
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

fn legal_config(source: &EnvironmentSource, env: Environment) -> Result<LegalConfig, ConfigError> {
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

fn fallback_value(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn legal_url(value: &str, name: &'static str, env: Environment) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(name))?;
    if url.scheme() != "https" && (env == Environment::Production || url.scheme() != "http") {
        Err(ConfigError::invalid(name))
    } else {
        Ok(url)
    }
}

fn billing_config(
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

fn limit(
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            format!("{:?}", config.jwt.secret),
            "SecretString([REDACTED])"
        );
        assert!(!format!("{config:?}").contains("jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj"));
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
        assert!(
            matches!(config.trust_proxy, TrustProxy::Networks(ref entries) if entries.len() == 3)
        );
        assert_eq!(config.cors_origins.len(), 2);
    }
}
