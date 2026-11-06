//! Atomic import persistence; observations never overwrite manual decisions.
use super::{
    domain::{ImportedNumber, MAX_IMPORT_ENTRIES, POLICY_LOCK, PolicyError, Source},
    pg::{key_fingerprint, verify_key_on},
    queries,
};
use crate::{
    config::SecretString,
    infra::postgres::{Database, map_sqlx_error},
};
use uuid::Uuid;

pub async fn import(
    database: &Database,
    source: Source,
    numbers: Vec<ImportedNumber>,
    key: &SecretString,
) -> Result<usize, PolicyError> {
    let count = numbers.len();
    if count > MAX_IMPORT_ENTRIES {
        return Err(PolicyError::Invalid);
    }
    let fingerprint = key_fingerprint(key)?;
    let mut ids = Vec::with_capacity(count);
    let mut hashes = Vec::with_capacity(count);
    let mut hints = Vec::with_capacity(count);
    let mut dates = Vec::with_capacity(count);
    for number in numbers {
        ids.push(Uuid::new_v4());
        hashes.push(number.key.hash);
        hints.push(number.key.hint);
        dates.push(number.last_seen_at);
    }
    database
        .transaction(|connection| {
            Box::pin(async move {
                sqlx::query(queries::EXCLUSIVE_LOCK)
                    .bind(POLICY_LOCK)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                verify_key_on(connection, &fingerprint).await?;
                sqlx::query(queries::IMPORT_ENTRIES)
                    .bind(ids)
                    .bind(&hashes)
                    .bind(hints)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                sqlx::query(queries::IMPORT_OBSERVATIONS)
                    .bind(source.name())
                    .bind(hashes)
                    .bind(dates)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                sqlx::query(queries::SOURCE_SUCCEEDED)
                    .bind(source.name())
                    .bind(count as i32)
                    .execute(connection)
                    .await
                    .map_err(map_sqlx_error)?;
                Ok(count)
            })
        })
        .await
}

pub async fn record_failure(database: &Database, source: Source) -> Result<(), PolicyError> {
    sqlx::query(queries::SOURCE_FAILED)
        .bind(source.name())
        .execute(database.pool())
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}
