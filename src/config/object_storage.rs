use super::{ConfigError, EnvironmentSource, types::*, validation::*};

impl ObjectStorageConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        let source = EnvironmentSource::from_pairs(std::env::vars_os());
        let environment = parse_environment(source.value("ENV")?.as_deref())?;
        Self::from_source(&source, environment)
    }

    pub(super) fn from_source(
        source: &EnvironmentSource,
        environment: Environment,
    ) -> Result<Self, ConfigError> {
        let (default_access, default_secret) = match environment {
            Environment::Production => ("", ""),
            _ => ("histae-dev", "histae-dev-secret-change-me"),
        };
        let access_key = source.or("OBJECT_STORAGE_ACCESS_KEY", default_access)?;
        let secret_key = source.or("OBJECT_STORAGE_SECRET_KEY", default_secret)?;
        if access_key.is_empty() || secret_key.is_empty() {
            return Err(ConfigError::missing("OBJECT_STORAGE_CREDENTIALS"));
        }
        Ok(Self {
            endpoint: object_storage_endpoint(
                &source.or(
                    "OBJECT_STORAGE_ENDPOINT",
                    "http://storage.histae.localhost:8333",
                )?,
                environment,
            )?,
            region: storage_region(&source.or("OBJECT_STORAGE_REGION", "us-east-1")?)?,
            bucket: storage_bucket(&source.or("OBJECT_STORAGE_BUCKET", "histae-photos")?)?,
            access_key,
            secret_key: SecretString::new(secret_key),
            force_path_style: boolean(
                source.value("OBJECT_STORAGE_FORCE_PATH_STYLE")?.as_deref(),
                true,
                "OBJECT_STORAGE_FORCE_PATH_STYLE",
            )?,
        })
    }
}
