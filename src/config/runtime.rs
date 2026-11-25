use super::{ConfigError, EnvironmentSource, types::*, validation::*};
use regex::Regex;
use std::{path::PathBuf, time::Duration};

pub(super) fn sms(
    source: &EnvironmentSource,
    environment: Environment,
    delivery: bool,
    webhook: bool,
) -> Result<SmsConfig, ConfigError> {
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
    let sms_api_key = if delivery {
        source.or("SWEEGO_API_KEY", "")?
    } else {
        String::new()
    };
    let sms_sender = if delivery {
        source.or("SWEEGO_SMS_SENDER_ID", "")?
    } else {
        String::new()
    };
    if delivery
        && sms_provider == SmsProvider::Sweego
        && (sms_api_key.is_empty() || sms_sender.is_empty())
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
    let webhook_secret = if webhook {
        source.or("SWEEGO_WEBHOOK_SECRET", "")?
    } else {
        String::new()
    };
    let resend_cooldown = duration(
        &source.or("OTP_RESEND_COOLDOWN", "60s")?,
        "OTP_RESEND_COOLDOWN",
    )?;
    ensure_duration(
        resend_cooldown,
        Duration::from_secs(60),
        Duration::from_secs(300),
        "OTP_RESEND_COOLDOWN",
    )?;
    if resend_cooldown > otp_ttl {
        return Err(ConfigError::invalid("OTP_RESEND_COOLDOWN"));
    }
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
        resend_cooldown,
        webhook_secret: SecretString::new(webhook_secret),
        safety: SmsSafetyConfig {
            hourly_global: integer(
                &source.or("SMS_MAX_GLOBAL_HOUR", "100")?,
                "SMS_MAX_GLOBAL_HOUR",
                1,
                1000000,
            )? as u32,
            daily_global: integer(
                &source.or("SMS_MAX_GLOBAL_DAY", "500")?,
                "SMS_MAX_GLOBAL_DAY",
                1,
                10000000,
            )? as u32,
            hourly_per_phone: integer(
                &source.or("SMS_MAX_PER_PHONE_HOUR", "5")?,
                "SMS_MAX_PER_PHONE_HOUR",
                1,
                1000,
            )? as u32,
            daily_per_phone: integer(
                &source.or("SMS_MAX_PER_PHONE_DAY", "10")?,
                "SMS_MAX_PER_PHONE_DAY",
                1,
                10000,
            )? as u32,
            spike_per_5_minutes: integer(
                &source.or("SMS_ALERT_SPIKE_5M", "20")?,
                "SMS_ALERT_SPIKE_5M",
                1,
                1000000,
            )? as u32,
            suspended: boolean(
                source.value("SMS_SENDING_SUSPENDED")?.as_deref(),
                false,
                "SMS_SENDING_SUSPENDED",
            )?,
        },
    };
    if sms.safety.daily_per_phone < sms.safety.hourly_per_phone {
        return Err(ConfigError::invalid("SMS_MAX_PER_PHONE_DAY"));
    }
    if sms.safety.daily_global < sms.safety.hourly_global {
        return Err(ConfigError::invalid("SMS_MAX_GLOBAL_DAY"));
    }
    Ok(sms)
}

pub(super) fn redis(
    source: &EnvironmentSource,
    environment: Environment,
    address_required: bool,
) -> Result<RedisConfig, ConfigError> {
    let configured_redis_address = source.or("REDIS_ADDR", "")?;
    if address_required && configured_redis_address.is_empty() {
        return Err(ConfigError::missing("REDIS_ADDR"));
    }
    let redis_address = if configured_redis_address.is_empty() {
        "localhost:6379".to_owned()
    } else {
        configured_redis_address
    };
    let redis_regex =
        Regex::new(r"^[a-zA-Z0-9._-]+:\d{1,5}$").map_err(|_| ConfigError::invalid("REDIS_ADDR"))?;
    if !redis_regex.is_match(&redis_address) {
        return Err(ConfigError::invalid("REDIS_ADDR"));
    }
    let redis_password = source.raw_value("REDIS_PASSWORD")?.unwrap_or_default();
    let redis_tls = boolean(source.value("REDIS_TLS")?.as_deref(), false, "REDIS_TLS")?;
    if environment == Environment::Production && (!redis_tls || redis_password.is_empty()) {
        return Err(ConfigError::invalid("REDIS_SECURITY"));
    }
    Ok(RedisConfig {
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
    })
}

pub(super) fn push(source: &EnvironmentSource) -> Result<PushConfig, ConfigError> {
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
    Ok(PushConfig {
        provider: push_provider,
        project_id,
        client_email,
        private_key: SecretString::new(private_key),
        token_uri: https_url(
            &source.or("FIREBASE_TOKEN_URI", "https://oauth2.googleapis.com/token")?,
            "FIREBASE_TOKEN_URI",
        )?,
        timeout: push_timeout,
    })
}

pub(super) fn maintenance_mode(
    source: &EnvironmentSource,
    environment: Environment,
) -> Result<MaintenanceMode, ConfigError> {
    Ok(
        match source
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
        },
    )
}

pub(super) fn workloads(source: &EnvironmentSource) -> Result<WorkloadConfig, ConfigError> {
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
        data_export_spool_dir: source
            .value("DATA_EXPORT_SPOOL_DIR")?
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        data_export_download_timeout: duration(
            &source.or("DATA_EXPORT_DOWNLOAD_TIMEOUT", "5m")?,
            "DATA_EXPORT_DOWNLOAD_TIMEOUT",
        )?,
    };
    let spool_bytes = integer(
        &source.or("DATA_EXPORT_SPOOL_BYTES", "2147483648")?,
        "DATA_EXPORT_SPOOL_BYTES",
        1_048_576,
        64 * 1024 * 1024 * 1024,
    )?;
    if assisted_export_max_bytes(workloads.data_export_max_bytes)
        * u64::from(workloads.data_export_max_concurrency)
        > spool_bytes
    {
        return Err(ConfigError::invalid("DATA_EXPORT_SPOOL_BYTES"));
    }
    if workloads
        .data_export_spool_dir
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(ConfigError::invalid("DATA_EXPORT_SPOOL_DIR"));
    }
    ensure_duration(
        workloads.data_export_download_timeout,
        Duration::from_secs(5),
        Duration::from_secs(1800),
        "DATA_EXPORT_DOWNLOAD_TIMEOUT",
    )?;
    Ok(workloads)
}

pub struct OutboxConfig {
    pub environment: Environment,
    pub maintenance_mode: MaintenanceMode,
    pub postgres: PostgresConfig,
    pub redis: RedisConfig,
    pub object_storage: ObjectStorageConfig,
    pub phone: PhoneConfig,
    pub sms: SmsConfig,
    pub push: PushConfig,
    pub store_billing: StoreBillingConfig,
    pub legal: LegalConfig,
    pub workloads: WorkloadConfig,
}

impl OutboxConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let source = EnvironmentSource::runtime();
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        let mut postgres = super::database::load(&source, environment)?;
        postgres.application_name = "histae-outbox";
        let encryption_key = source.required("PHONE_ENCRYPTION_KEY")?;
        let hash_key = source.required("PHONE_HASH_KEY")?;
        if phone_key_bytes(&encryption_key, "PHONE_ENCRYPTION_KEY")?
            == phone_key_bytes(&hash_key, "PHONE_HASH_KEY")?
        {
            return Err(ConfigError::conflict("PHONE_KEYS"));
        }
        Ok(Self {
            environment,
            maintenance_mode: maintenance_mode(&source, environment)?,
            postgres,
            redis: redis(&source, environment, environment == Environment::Production)?,
            object_storage: ObjectStorageConfig::from_source(&source, environment)?,
            phone: PhoneConfig {
                encryption_key: SecretString::new(encryption_key),
                hash_key: SecretString::new(hash_key),
            },
            sms: sms(&source, environment, true, false)?,
            push: push(&source)?,
            store_billing: super::stores::load(&source, environment)?,
            legal: legal_config(&source, environment)?,
            workloads: workloads(&source)?,
        })
    }
}

pub struct MaintenanceConfig {
    pub environment: Environment,
    pub maintenance_mode: MaintenanceMode,
    pub postgres: PostgresConfig,
    pub object_storage: ObjectStorageConfig,
    pub schedule_store_reconciliation: bool,
    pub workloads: WorkloadConfig,
}

impl MaintenanceConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let source = EnvironmentSource::runtime();
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        let mut postgres = super::database::load(&source, environment)?;
        postgres.application_name = "histae-maintenance";
        let flag = |name| boolean(source.value(name)?.as_deref(), false, name);
        let apple_enabled = flag("APP_STORE_ENABLED")?;
        let google_enabled = flag("GOOGLE_PLAY_ENABLED")?;
        Ok(Self {
            environment,
            maintenance_mode: maintenance_mode(&source, environment)?,
            postgres,
            object_storage: ObjectStorageConfig::from_source(&source, environment)?,
            schedule_store_reconciliation: apple_enabled || google_enabled,
            workloads: workloads(&source)?,
        })
    }
}

pub struct MigrationConfig {
    pub postgres: PostgresConfig,
    pub phone_hash_key: SecretString,
}

impl MigrationConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let source = EnvironmentSource::runtime();
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        let mut postgres = super::database::load(&source, environment)?;
        postgres.application_name = "histae-migrate";
        let key = source.required("PHONE_HASH_KEY")?;
        phone_key_bytes(&key, "PHONE_HASH_KEY")?;
        Ok(Self {
            postgres,
            phone_hash_key: SecretString::new(key),
        })
    }
}
