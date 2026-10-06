use sqlx::{Acquire, Executor};

use crate::config::PostgresConfig;

use super::postgres::{
    DatabaseError, connect_pool, map_sqlx_error, verify_schema_compatibility_on,
};

const MIGRATION_LOCK: i64 = 86_302_003;
const SCHEMA: &str = include_str!("../../db/001_schema_postgres.sql");
const SEED: &str = include_str!("../../db/002_insert_postgres.sql");

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
    drop(connection);
    pool.close().await;
    match result {
        Ok(count) => unlock.map(|_| count),
        Err(error) => Err(error),
    }
}

async fn apply_locked(connection: &mut sqlx::PgConnection) -> Result<u32, DatabaseError> {
    let populated: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = current_schema() AND c.relname <> 'schema_migrations'
         AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f'))",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;

    if populated {
        upgrade_existing_schema(connection).await?;
        return Ok(0);
    }

    let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
    sqlx::query("SELECT set_config('histae.seed_fake_users', 'off', true)")
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
    transaction
        .execute(sqlx::raw_sql(SCHEMA))
        .await
        .map_err(map_sqlx_error)?;
    transaction
        .execute(sqlx::raw_sql(SEED))
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DROP TABLE IF EXISTS schema_migrations")
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
    transaction.commit().await.map_err(map_sqlx_error)?;
    Ok(1)
}

async fn upgrade_existing_schema(connection: &mut sqlx::PgConnection) -> Result<(), DatabaseError> {
    let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
    verify_schema_compatibility_on(&mut transaction).await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_user_account_single_superadmin
         ON user_account (role) WHERE role = 'superadmin'",
    )
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    let role_actions_present: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint
         WHERE conrelid = to_regclass(current_schema() || '.data_access_log')
           AND conname = 'data_access_log_action_check'
           AND pg_get_constraintdef(oid) LIKE '%admin_promote%'
           AND pg_get_constraintdef(oid) LIKE '%admin_demote%')",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if !role_actions_present {
        sqlx::query("ALTER TABLE data_access_log DROP CONSTRAINT data_access_log_action_check")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query(
            "ALTER TABLE data_access_log ADD CONSTRAINT data_access_log_action_check
             CHECK (action IN (
               'view_profile', 'view_messages', 'view_matches', 'export_data',
               'admin_ban', 'admin_unban', 'admin_promote', 'admin_demote',
               'admin_review_report', 'admin_review_dsr', 'admin_reconcile_photo',
               'view_moderation_content', 'admin_review_content',
               'system_anonymize', 'system_export_portability'))",
        )
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
    }
    sqlx::query("DROP TABLE IF EXISTS schema_migrations")
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
    transaction.commit().await.map_err(map_sqlx_error)
}
