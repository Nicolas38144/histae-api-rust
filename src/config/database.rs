use super::{ConfigError, EnvironmentSource, types::*, validation::*};
use std::path::PathBuf;

pub(super) fn load(
    source: &EnvironmentSource,
    environment: Environment,
) -> Result<PostgresConfig, ConfigError> {
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

    Ok(postgres)
}
