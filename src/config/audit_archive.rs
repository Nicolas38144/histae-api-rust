use std::path::PathBuf;

use super::{ConfigError, EnvironmentSource, PostgresConfig, database, validation};

pub struct AuditArchiveConfig {
    pub postgres: PostgresConfig,
    pub directory: PathBuf,
}

impl AuditArchiveConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let source = EnvironmentSource::from_pairs(std::env::vars_os());
        let environment = validation::parse_environment(source.value("ENV")?.as_deref())?;
        let postgres = database::load(&source, environment)?;
        if postgres.user != "histae_audit_archive" {
            return Err(ConfigError::invalid("POSTGRES_USER"));
        }
        let directory = PathBuf::from(source.required("AUDIT_ARCHIVE_DIR")?);
        if !directory.is_absolute() {
            return Err(ConfigError::invalid("AUDIT_ARCHIVE_DIR"));
        }
        Ok(Self {
            postgres,
            directory,
        })
    }
}
