use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use log::LevelFilter;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{Acquire, ConnectOptions, PgConnection, PgPool, Postgres};

use crate::config::PostgresConfig;

pub const EXPECTED_MIGRATIONS: &[MigrationFingerprint] = &[
    MigrationFingerprint {
        version: "001_baseline_20260905",
        checksum: "7d33ff78d8094576acc30af275e1426f2feb6333911283ef1f1aadf2f9b8e111",
    },
    MigrationFingerprint {
        version: "017_postgres_discovery",
        checksum: "f2e656a133d64a08873c86cb9a4dddbc84c4c1704e590851ed63a3a2ac6d1006",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationFingerprint {
    pub version: &'static str,
    pub checksum: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MigrationHistoryRow {
    version: String,
    checksum: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConstraintKind {
    NotNull,
    ForeignKey,
    Unique,
    Check,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseError {
    AccountUnavailable,
    Constraint(ConstraintKind),
    SerializationFailure,
    Deadlock,
    RowNotFound,
    PoolTimedOut,
    ConnectionFailed,
    QueryFailed,
    TransactionBeginFailed,
    TransactionCommitFailed,
    TransactionRollbackFailed,
    MigrationHistoryMissing,
    UnknownMigration,
    MissingMigration(&'static str),
    MigrationChecksumMissing(&'static str),
    MigrationChecksumMismatch(&'static str),
    SchemaObjectsMissing,
}

impl DatabaseError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::AccountUnavailable => "account_unavailable",
            Self::Constraint(ConstraintKind::NotNull) => "postgres_not_null_violation",
            Self::Constraint(ConstraintKind::ForeignKey) => "postgres_foreign_key_violation",
            Self::Constraint(ConstraintKind::Unique) => "postgres_unique_violation",
            Self::Constraint(ConstraintKind::Check) => "postgres_check_violation",
            Self::SerializationFailure => "postgres_serialization_failure",
            Self::Deadlock => "postgres_deadlock",
            Self::RowNotFound => "postgres_row_not_found",
            Self::PoolTimedOut => "postgres_pool_timeout",
            Self::ConnectionFailed => "postgres_connection_failed",
            Self::QueryFailed => "postgres_query_failed",
            Self::TransactionBeginFailed => "postgres_transaction_begin_failed",
            Self::TransactionCommitFailed => "postgres_transaction_commit_failed",
            Self::TransactionRollbackFailed => "postgres_transaction_rollback_failed",
            Self::MigrationHistoryMissing => "postgres_migration_history_missing",
            Self::UnknownMigration => "postgres_unknown_migration",
            Self::MissingMigration(_) => "postgres_migration_missing",
            Self::MigrationChecksumMissing(_) => "postgres_migration_checksum_missing",
            Self::MigrationChecksumMismatch(_) => "postgres_migration_checksum_mismatch",
            Self::SchemaObjectsMissing => "postgres_schema_objects_missing",
        }
    }

    pub fn sqlstate(self) -> Option<&'static str> {
        match self {
            Self::AccountUnavailable => Some("P0E01"),
            Self::Constraint(ConstraintKind::NotNull) => Some("23502"),
            Self::Constraint(ConstraintKind::ForeignKey) => Some("23503"),
            Self::Constraint(ConstraintKind::Unique) => Some("23505"),
            Self::Constraint(ConstraintKind::Check) => Some("23514"),
            Self::SerializationFailure => Some("40001"),
            Self::Deadlock => Some("40P01"),
            _ => None,
        }
    }

    pub fn is_retryable(self) -> bool {
        matches!(self, Self::SerializationFailure | Self::Deadlock)
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_code())
    }
}

impl std::error::Error for DatabaseError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolStats {
    pub total: u32,
    pub idle: u32,
    pub waiting: u32,
    pub max: u32,
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
    waiting: Arc<AtomicU32>,
    max_connections: u32,
}

pub type TransactionFuture<'connection, T, E> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'connection>>;

impl Database {
    pub async fn connect(config: &PostgresConfig) -> Result<Self, DatabaseError> {
        let pool = connect_pool(config, config.max_connections, config.application_name).await?;
        let database = Self {
            pool,
            waiting: Arc::new(AtomicU32::new(0)),
            max_connections: config.max_connections,
        };
        database.ping().await?;
        database.verify_schema_compatibility().await?;
        Ok(database)
    }

    pub async fn ping(&self) -> Result<(), DatabaseError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(map_sqlx_error)
    }

    pub async fn acquire(&self) -> Result<sqlx::pool::PoolConnection<Postgres>, DatabaseError> {
        let waiting = WaitingAcquisition::new(&self.waiting);
        let connection = self.pool.acquire().await.map_err(map_sqlx_error);
        drop(waiting);
        connection
    }

    pub async fn transaction<T, E, F>(&self, operation: F) -> Result<T, E>
    where
        T: Send,
        E: From<DatabaseError>,
        F: for<'connection> FnOnce(
            &'connection mut PgConnection,
        ) -> TransactionFuture<'connection, T, E>,
    {
        let mut connection = self.acquire().await.map_err(E::from)?;
        let mut transaction = connection
            .begin()
            .await
            .map_err(|error| E::from(map_transaction_error(error, TransactionPhase::Begin)))?;
        let result = operation(&mut transaction).await;
        match result {
            Ok(value) => {
                transaction.commit().await.map_err(|error| {
                    E::from(map_transaction_error(error, TransactionPhase::Commit))
                })?;
                Ok(value)
            }
            Err(operation_error) => {
                transaction.rollback().await.map_err(|error| {
                    E::from(map_transaction_error(error, TransactionPhase::Rollback))
                })?;
                Err(operation_error)
            }
        }
    }

    pub async fn verify_schema_compatibility(&self) -> Result<(), DatabaseError> {
        let mut connection = self.acquire().await?;
        verify_schema_compatibility_on(&mut connection).await
    }

    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            total: self.pool.size(),
            idle: u32::try_from(self.pool.num_idle()).unwrap_or(u32::MAX),
            waiting: self.waiting.load(Ordering::Relaxed),
            max: self.max_connections,
        }
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

pub(super) async fn connect_pool(
    config: &PostgresConfig,
    max_connections: u32,
    application_name: &'static str,
) -> Result<PgPool, DatabaseError> {
    let mut options = PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.user)
        .password(config.password.expose_secret())
        .database(&config.database)
        .application_name(application_name)
        .ssl_mode(if config.tls {
            PgSslMode::VerifyFull
        } else {
            PgSslMode::Disable
        })
        .options([
            (
                "statement_timeout",
                config.statement_timeout.as_millis().to_string(),
            ),
            (
                "idle_in_transaction_session_timeout",
                config.idle_transaction_timeout.as_millis().to_string(),
            ),
        ]);
    if let Some(root_certificate) = &config.root_certificate {
        options = options.ssl_root_cert(root_certificate);
    }
    options = options.log_statements(LevelFilter::Off);

    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(config.connect_timeout)
        .idle_timeout(Some(config.idle_timeout))
        .test_before_acquire(true)
        .connect_with(options)
        .await
        .map_err(map_sqlx_error)
}

pub async fn verify_schema_compatibility_on(
    connection: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let history_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass(current_schema() || '.schema_migrations')::text")
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
    if history_table.is_none() {
        return Err(DatabaseError::MigrationHistoryMissing);
    }

    let history = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT version, checksum FROM schema_migrations",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?
    .into_iter()
    .map(|(version, checksum)| MigrationHistoryRow { version, checksum })
    .collect::<Vec<_>>();
    validate_migration_history(&history)?;

    let objects_present: bool = sqlx::query_scalar(
        "SELECT to_regclass(current_schema() || '.user_account') IS NOT NULL
            AND to_regclass(current_schema() || '.swipe_decision') IS NOT NULL",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if !objects_present {
        return Err(DatabaseError::SchemaObjectsMissing);
    }
    Ok(())
}

struct WaitingAcquisition<'counter> {
    counter: &'counter AtomicU32,
}

impl<'counter> WaitingAcquisition<'counter> {
    fn new(counter: &'counter AtomicU32) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for WaitingAcquisition<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

fn validate_migration_history(history: &[MigrationHistoryRow]) -> Result<(), DatabaseError> {
    if history.is_empty() {
        return Err(DatabaseError::MigrationHistoryMissing);
    }
    for row in history {
        let Some(expected) = EXPECTED_MIGRATIONS
            .iter()
            .find(|migration| migration.version == row.version)
        else {
            return Err(DatabaseError::UnknownMigration);
        };
        let Some(checksum) = row.checksum.as_deref() else {
            return Err(DatabaseError::MigrationChecksumMissing(expected.version));
        };
        if checksum != expected.checksum {
            return Err(DatabaseError::MigrationChecksumMismatch(expected.version));
        }
    }
    for expected in EXPECTED_MIGRATIONS {
        if !history.iter().any(|row| row.version == expected.version) {
            return Err(DatabaseError::MissingMigration(expected.version));
        }
    }
    Ok(())
}

pub fn map_sqlstate(code: Option<&str>) -> DatabaseError {
    match code {
        Some("P0E01") => DatabaseError::AccountUnavailable,
        Some("23502") => DatabaseError::Constraint(ConstraintKind::NotNull),
        Some("23503") => DatabaseError::Constraint(ConstraintKind::ForeignKey),
        Some("23505") => DatabaseError::Constraint(ConstraintKind::Unique),
        Some("23514") => DatabaseError::Constraint(ConstraintKind::Check),
        Some("40001") => DatabaseError::SerializationFailure,
        Some("40P01") => DatabaseError::Deadlock,
        _ => DatabaseError::QueryFailed,
    }
}

pub fn map_sqlx_error(error: sqlx::Error) -> DatabaseError {
    match error {
        sqlx::Error::PoolTimedOut => DatabaseError::PoolTimedOut,
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed => {
            DatabaseError::ConnectionFailed
        }
        sqlx::Error::RowNotFound => DatabaseError::RowNotFound,
        sqlx::Error::Database(database) => map_sqlstate(database.code().as_deref()),
        _ => DatabaseError::QueryFailed,
    }
}

#[derive(Clone, Copy)]
enum TransactionPhase {
    Begin,
    Commit,
    Rollback,
}

fn map_transaction_error(error: sqlx::Error, phase: TransactionPhase) -> DatabaseError {
    let mapped = map_sqlx_error(error);
    if matches!(
        mapped,
        DatabaseError::AccountUnavailable
            | DatabaseError::Constraint(_)
            | DatabaseError::SerializationFailure
            | DatabaseError::Deadlock
    ) {
        return mapped;
    }
    match phase {
        TransactionPhase::Begin => DatabaseError::TransactionBeginFailed,
        TransactionPhase::Commit => DatabaseError::TransactionCommitFailed,
        TransactionPhase::Rollback => DatabaseError::TransactionRollbackFailed,
    }
}

pub fn duration_setting(value: &str) -> Result<std::time::Duration, DatabaseError> {
    let normalized = value.trim();
    if normalized == "0" {
        return Ok(std::time::Duration::ZERO);
    }
    let (number, multiplier) = if let Some(number) = normalized.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = normalized.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = normalized.strip_suffix("min") {
        (number, 60_000)
    } else {
        return Err(DatabaseError::QueryFailed);
    };
    let number = u64::from_str(number.trim()).map_err(|_| DatabaseError::QueryFailed)?;
    number
        .checked_mul(multiplier)
        .map(std::time::Duration::from_millis)
        .ok_or(DatabaseError::QueryFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_history() -> Vec<MigrationHistoryRow> {
        EXPECTED_MIGRATIONS
            .iter()
            .map(|migration| MigrationHistoryRow {
                version: migration.version.to_owned(),
                checksum: Some(migration.checksum.to_owned()),
            })
            .collect()
    }

    #[test]
    fn accepts_only_the_current_complete_migration_history() {
        assert_eq!(validate_migration_history(&valid_history()), Ok(()));

        let mut unknown = valid_history();
        unknown.push(MigrationHistoryRow {
            version: "999_unknown".to_owned(),
            checksum: Some("invalid".to_owned()),
        });
        assert_eq!(
            validate_migration_history(&unknown),
            Err(DatabaseError::UnknownMigration)
        );

        let mut missing = valid_history();
        missing.pop();
        assert_eq!(
            validate_migration_history(&missing),
            Err(DatabaseError::MissingMigration("017_postgres_discovery"))
        );
    }

    #[test]
    fn rejects_missing_or_changed_checksums_without_retaining_the_value() {
        let mut missing = valid_history();
        missing[0].checksum = None;
        assert_eq!(
            validate_migration_history(&missing),
            Err(DatabaseError::MigrationChecksumMissing(
                "001_baseline_20260905"
            ))
        );

        let mut changed = valid_history();
        changed[1].checksum = Some("private-or-corrupt-value".to_owned());
        let error = validate_migration_history(&changed)
            .expect_err("a modified migration checksum must be refused");
        assert_eq!(
            error,
            DatabaseError::MigrationChecksumMismatch("017_postgres_discovery")
        );
        assert!(!format!("{error:?}").contains("private-or-corrupt-value"));
    }

    #[test]
    fn maps_publicly_relevant_sqlstates_without_database_details() {
        assert_eq!(
            map_sqlstate(Some("P0E01")),
            DatabaseError::AccountUnavailable
        );
        assert_eq!(
            map_sqlstate(Some("23505")),
            DatabaseError::Constraint(ConstraintKind::Unique)
        );
        assert_eq!(
            map_sqlstate(Some("23503")),
            DatabaseError::Constraint(ConstraintKind::ForeignKey)
        );
        assert_eq!(
            map_sqlstate(Some("40001")),
            DatabaseError::SerializationFailure
        );
        assert_eq!(map_sqlstate(Some("XX000")), DatabaseError::QueryFailed);
        assert_eq!(DatabaseError::AccountUnavailable.sqlstate(), Some("P0E01"));
        assert!(DatabaseError::Deadlock.is_retryable());
    }

    #[test]
    fn parses_postgres_timeout_settings_without_loss() {
        assert_eq!(
            duration_setting("15000ms"),
            Ok(std::time::Duration::from_secs(15))
        );
        assert_eq!(
            duration_setting("30s"),
            Ok(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            duration_setting("1min"),
            Ok(std::time::Duration::from_secs(60))
        );
        assert_eq!(duration_setting("0"), Ok(std::time::Duration::ZERO));
        assert_eq!(duration_setting("private"), Err(DatabaseError::QueryFailed));
    }
}
