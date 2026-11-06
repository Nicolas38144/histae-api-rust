use super::{ConfigError, EnvironmentSource, PostgresConfig, SecretString, validation::*};
pub struct PhoneBlocklistSyncConfig {
    pub postgres: PostgresConfig,
    pub hash_key: SecretString,
}

impl PhoneBlocklistSyncConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&EnvironmentSource::from_pairs(std::env::vars_os()))
    }
    pub fn from_source(source: &EnvironmentSource) -> Result<Self, ConfigError> {
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        let mut postgres = super::database::load(source, environment)?;
        if postgres.user != "histae_phone_sync" {
            return Err(ConfigError::invalid("POSTGRES_USER"));
        }
        postgres.max_connections = 2;
        postgres.application_name = "histae-phone-sync";
        let raw = source.required("PHONE_HASH_KEY")?;
        phone_key_bytes(&raw, "PHONE_HASH_KEY")?;
        Ok(Self {
            postgres,
            hash_key: SecretString::new(raw),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(user: &str, environment: &str, tls: &str) -> EnvironmentSource {
        EnvironmentSource::from_pairs([
            ("ENV", environment),
            ("POSTGRES_HOST", "localhost"),
            ("POSTGRES_DB", "histae-dev"),
            ("POSTGRES_USER", user),
            ("POSTGRES_PASSWORD", "dedicated-synthetic-password"),
            ("POSTGRES_SSLMODE", tls),
            ("PHONE_HASH_KEY", "0123456789abcdef0123456789abcdef"),
        ])
    }
    #[test]
    fn sync_needs_only_its_own_credentials_and_refuses_broader_roles_or_production_without_tls() {
        let config = PhoneBlocklistSyncConfig::from_source(&source(
            "histae_phone_sync",
            "development",
            "disable",
        ))
        .unwrap();
        assert_eq!(config.postgres.max_connections, 2);
        for user in ["histae_admin", "histae_migrate", "histae_app"] {
            assert!(
                PhoneBlocklistSyncConfig::from_source(&source(user, "development", "disable"))
                    .is_err()
            );
        }
        assert!(
            PhoneBlocklistSyncConfig::from_source(&source(
                "histae_phone_sync",
                "production",
                "disable"
            ))
            .is_err()
        );
        assert!(
            PhoneBlocklistSyncConfig::from_source(&source(
                "histae_phone_sync",
                "production",
                "verify-full"
            ))
            .is_ok()
        );
    }
}
