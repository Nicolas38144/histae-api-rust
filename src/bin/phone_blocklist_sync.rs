use histae_api_rust::{
    config::PhoneBlocklistSyncConfig,
    operations::logging::{self, SafeLogValue},
};
use std::process::ExitCode;
#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments == [std::ffi::OsString::from("--help")] {
        println!("Usage: phone-blocklist-sync [--check-config]");
        return ExitCode::SUCCESS;
    }
    let check = arguments == [std::ffi::OsString::from("--check-config")];
    if !arguments.is_empty() && !check {
        let _ = logging::error(
            "phone_blocklist_sync_failed",
            Some("invalid_command_arguments"),
        );
        return ExitCode::FAILURE;
    }
    let config = match PhoneBlocklistSyncConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            let _ = logging::error("phone_blocklist_sync_failed", Some(e.safe_code()));
            return ExitCode::FAILURE;
        }
    };
    if check {
        let _ = logging::info(
            "configuration_validated",
            &[("operation", SafeLogValue::String("phone_blocklist_sync"))],
        );
        return ExitCode::SUCCESS;
    }
    match histae_api_rust::app::phone_blocklist::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let _ = logging::error("phone_blocklist_sync_failed", Some(code));
            ExitCode::FAILURE
        }
    }
}
