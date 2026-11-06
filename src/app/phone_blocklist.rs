//! One-off public phone source synchronization with minimal credentials.
use crate::{
    config::{PhoneBlocklistSyncConfig, SecretString},
    identity::mobile::phone_blocklist::{domain::Source, pg, sync},
    infra::postgres::Database,
    operations::logging::{self, SafeLogValue},
};

pub async fn run(config: PhoneBlocklistSyncConfig) -> Result<(), &'static str> {
    let database = Database::connect(&config.postgres)
        .await
        .map_err(|error| error.safe_code())?;
    let result = synchronize(&database, &config.hash_key).await;
    database.close().await;
    result
}

async fn synchronize(database: &Database, hash_key: &SecretString) -> Result<(), &'static str> {
    if let Err(error) = pg::check_key(database, hash_key).await {
        for source in Source::ALL {
            let _ = pg::record_failure(database, source).await;
        }
        return Err(error.safe_code());
    }
    let mut successes = 0;
    for source in Source::ALL {
        let result = match sync::fetch(source, hash_key).await {
            Ok(numbers) => pg::import(database, source, numbers, hash_key).await,
            Err(error) => Err(error),
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
                let _ = pg::record_failure(database, source).await;
                let _ = logging::error(
                    "phone_blocklist_sync_failed",
                    Some("phone_feed_unavailable"),
                );
            }
        }
    }
    if successes > 0 {
        Ok(())
    } else {
        Err("phone_feed_unavailable")
    }
}
