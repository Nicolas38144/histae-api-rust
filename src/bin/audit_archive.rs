use std::process::ExitCode;

use histae_api_rust::{config::AuditArchiveConfig, infra::audit_archive, operations::logging};

#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let result = match AuditArchiveConfig::from_env() {
        Ok(config) => audit_archive::run(config).await,
        Err(error) => Err(error.safe_code()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let _ = logging::error("audit_archive_failed", Some(code));
            ExitCode::FAILURE
        }
    }
}
