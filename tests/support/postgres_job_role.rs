use histae_api_rust::{
    config::{PostgresConfig, SecretString},
    infra::postgres::{Database, DatabaseError},
};

// The isolated PostgreSQL runner supplies separate credentials. Preserve the
// native development workflow when these test credentials are not available.
pub async fn connect(config: &PostgresConfig, job: &str) -> Result<Database, DatabaseError> {
    let mut config = config.clone();
    if let Ok(password) =
        std::env::var(format!("HISTAE_TEST_{}_PASSWORD", job.to_ascii_uppercase()))
    {
        config.user = format!("histae_{job}");
        config.password = SecretString::new(password);
    }
    Database::connect(&config).await
}
