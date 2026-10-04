use std::process::ExitCode;

use histae_api_rust::config::AppConfig;
use histae_api_rust::media::S3ObjectStorage;
use histae_api_rust::operations::logging;

#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let config = match AppConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            let _ = logging::error(
                "object_storage_initialization_failed",
                Some(error.safe_code()),
            );
            return ExitCode::FAILURE;
        }
    };
    let result = match S3ObjectStorage::new(&config.object_storage) {
        Ok(storage) => storage.ensure_bucket().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => {
            let _ = logging::info("object_storage_initialized", &[]);
            ExitCode::SUCCESS
        }
        Err(_) => {
            let _ = logging::error(
                "object_storage_initialization_failed",
                Some("object_storage_unavailable"),
            );
            ExitCode::FAILURE
        }
    }
}
