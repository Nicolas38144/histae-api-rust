use std::process::ExitCode;

use histae_api_rust::config::AppConfig;
use histae_api_rust::infra::migrations;
use histae_api_rust::operations::logging::{self, SafeLogValue};

#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let config = match AppConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            let _ = logging::error("postgres_migration_failed", Some(error.safe_code()));
            return ExitCode::FAILURE;
        }
    };
    match migrations::apply(&config.postgres).await {
        Ok(count) => {
            let database =
                match histae_api_rust::infra::postgres::Database::connect(&config.postgres).await {
                    Ok(database) => database,
                    Err(error) => {
                        let _ =
                            logging::error("postgres_migration_failed", Some(error.safe_code()));
                        return ExitCode::FAILURE;
                    }
                };
            let binding = histae_api_rust::identity::mobile::phone_blocklist::pg::initialize_key(
                &database,
                &config.phone.hash_key,
            )
            .await;
            database.close().await;
            if let Err(error) = binding {
                let _ = logging::error("postgres_migration_failed", Some(error.safe_code()));
                return ExitCode::FAILURE;
            }

            let _ = logging::info(
                "postgres_migration_completed",
                &[("processed_count", SafeLogValue::Integer(i64::from(count)))],
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = logging::error("postgres_migration_failed", Some(error.safe_code()));
            ExitCode::FAILURE
        }
    }
}
