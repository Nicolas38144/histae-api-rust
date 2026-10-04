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
