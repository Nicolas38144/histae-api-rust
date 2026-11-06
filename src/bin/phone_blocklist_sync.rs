use histae_api_rust::{
    config::PhoneBlocklistSyncConfig,
    identity::mobile::phone_blocklist::sync::{self, Source},
    infra::postgres::Database,
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
    let database = match Database::connect(&config.postgres).await {
        Ok(d) => d,
        Err(e) => {
            let _ = logging::error("phone_blocklist_sync_failed", Some(e.safe_code()));
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = histae_api_rust::identity::mobile::phone_blocklist::pg::check_key(
        &database,
        &config.hash_key,
    )
    .await
    {
        for source in [Source::Castle, Source::Ip1] {
            let _ = sync::record_failure(&database, source).await;
        }
        let _ = logging::error("phone_blocklist_sync_failed", Some(error.safe_code()));
        database.close().await;
        return ExitCode::FAILURE;
    }
    let mut successes = 0;
    for source in [Source::Castle, Source::Ip1] {
        let result = match sync::fetch(source, &config.hash_key).await {
            Ok(numbers) => sync::import(&database, source, numbers, &config.hash_key).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(count) => {
                successes += 1;
                let _ = logging::info(
                    "phone_blocklist_sync_completed",
                    &[
                        ("operation", SafeLogValue::String(source.name())),
                        ("processed_count", SafeLogValue::Integer(count as i64)),
                    ],
                );
            }
            Err(_) => {
                let _ = sync::record_failure(&database, source).await;
                let _ = logging::error(
                    "phone_blocklist_sync_failed",
                    Some("phone_feed_unavailable"),
                );
            }
        }
    }
    database.close().await;
    if successes > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
