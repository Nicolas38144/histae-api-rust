use sqlx::{Acquire, Executor, Row};

use crate::config::PostgresConfig;

use super::postgres::{DatabaseError, connect_pool, map_sqlx_error};

const MIGRATION_LOCK: i64 = 86_302_003;

struct Migration {
    version: &'static str,
    checksum: &'static str,
    sources: &'static [&'static str],
}

const MIGRATIONS: [Migration; 3] = [
    Migration {
        version: "001_baseline_20260905",
        checksum: "7d33ff78d8094576acc30af275e1426f2feb6333911283ef1f1aadf2f9b8e111",
        sources: &[
            include_str!("../../db/001_schema_postgres.sql"),
            include_str!("../../db/002_insert_postgres.sql"),
        ],
    },
    Migration {
        version: "017_postgres_discovery",
        checksum: "f2e656a133d64a08873c86cb9a4dddbc84c4c1704e590851ed63a3a2ac6d1006",
        sources: &[include_str!("../../db/003_postgres_discovery.sql")],
    },
    Migration {
        version: "018_postgres_admin_webauthn_state",
        checksum: "7127cee30dbb61fc967e0864ffbe636a34a18fa44be9ea449ec0b035d9d8c95a",
        sources: &[include_str!(
            "../../db/004_postgres_admin_webauthn_state.sql"
        )],
    },
];

pub async fn apply(config: &PostgresConfig) -> Result<u32, DatabaseError> {
    let pool = connect_pool(config, 1, "histae-migrate").await?;
    let mut connection = pool.acquire().await.map_err(map_sqlx_error)?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let result = apply_locked(&mut connection).await;
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error);
    pool.close().await;
    match result {
        Ok(count) => unlock.map(|_| count),
        Err(error) => Err(error),
    }
}

async fn apply_locked(connection: &mut sqlx::PgConnection) -> Result<u32, DatabaseError> {
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations (version TEXT PRIMARY KEY, checksum TEXT NOT NULL, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())")
        .execute(&mut *connection).await.map_err(map_sqlx_error)?;
    let history = sqlx::query("SELECT version, checksum FROM schema_migrations")
        .fetch_all(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    for row in &history {
        let version: String = row
            .try_get("version")
            .map_err(|_| DatabaseError::QueryFailed)?;
        if !MIGRATIONS
            .iter()
            .any(|migration| migration.version == version)
        {
            return Err(DatabaseError::UnknownMigration);
        }
    }
    let mut applied = 0_u32;
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let existing = sqlx::query("SELECT checksum FROM schema_migrations WHERE version = $1")
            .bind(migration.version)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        if let Some(row) = existing {
            let checksum: String = row
                .try_get("checksum")
                .map_err(|_| DatabaseError::MigrationChecksumMissing(migration.version))?;
            if checksum != migration.checksum {
                return Err(DatabaseError::MigrationChecksumMismatch(migration.version));
            }
            continue;
        }
        let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
        if index == 0 {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname <> 'schema_migrations' AND c.relkind IN ('r','p','v','m','S','f'))",
            ).fetch_one(&mut *transaction).await.map_err(map_sqlx_error)?;
            if present {
                return Err(DatabaseError::SchemaObjectsMissing);
            }
            sqlx::query("SELECT set_config('histae.seed_fake_users', 'off', true)")
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx_error)?;
        }
        for source in migration.sources {
            transaction
                .execute(sqlx::raw_sql(source))
                .await
                .map_err(map_sqlx_error)?;
        }
        sqlx::query("INSERT INTO schema_migrations(version, checksum) VALUES ($1, $2)")
            .bind(migration.version)
            .bind(migration.checksum)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        transaction.commit().await.map_err(map_sqlx_error)?;
        applied = applied.saturating_add(1);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::postgres::EXPECTED_MIGRATIONS;
    #[test]
    fn embeds_the_frozen_catalog_in_order() {
        assert_eq!(MIGRATIONS.len(), EXPECTED_MIGRATIONS.len());
        for (migration, expected) in MIGRATIONS.iter().zip(EXPECTED_MIGRATIONS) {
            assert_eq!(
                (migration.version, migration.checksum),
                (expected.version, expected.checksum)
            );
            assert!(
                !migration
                    .sources
                    .iter()
                    .any(|source| source.trim().is_empty())
            );
        }
    }
}
