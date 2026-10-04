use std::{collections::BTreeMap, fmt, path::PathBuf, time::Duration};
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
    pub(super) fn as_bytes(&self) -> &[u8] {
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
    pub root_certificate: Option<PathBuf>,
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
