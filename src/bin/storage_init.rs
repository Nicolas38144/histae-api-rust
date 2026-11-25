use std::process::ExitCode;

use histae_api_rust::config::ObjectStorageConfig;
use histae_api_rust::media::{S3ObjectStorage, storage::PhotoObjectStorage};
use histae_api_rust::operations::logging;

enum BucketOperation {
    Check,
    Create,
}

#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let mut arguments = std::env::args_os().skip(1);
    let operation = match (arguments.next(), arguments.next()) {
        (None, None) => BucketOperation::Check,
        (Some(flag), None) if flag == "--create" => BucketOperation::Create,
        _ => {
            let _ = logging::error(
                "object_storage_initialization_failed",
                Some("invalid_arguments"),
            );
            return ExitCode::FAILURE;
        }
    };
    let config = match ObjectStorageConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            let _ = logging::error(
                "object_storage_initialization_failed",
                Some(error.safe_code()),
            );
            return ExitCode::FAILURE;
        }
    };
    let result = match (S3ObjectStorage::new(&config), operation) {
        (Ok(storage), BucketOperation::Check) => storage.check().await,
        (Ok(storage), BucketOperation::Create) => storage.ensure_bucket().await,
        (Err(error), _) => Err(error),
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
