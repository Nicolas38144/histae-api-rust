use sqlx::{Acquire, Executor};

use crate::config::PostgresConfig;

use super::postgres::{
    DatabaseError, casework_notifications_ready_on, connect_pool, map_sqlx_error,
    verify_baseline_schema_on, verify_schema_compatibility_on,
};

const MIGRATION_LOCK: i64 = 86_302_003;
const SCHEMA: &str = include_str!("../../db/001_schema_postgres.sql");
const SEED: &str = include_str!("../../db/002_insert_postgres.sql");
const CASEWORK_NOTIFICATIONS: &str = include_str!("../../db/003_casework_notifications.sql");

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
         WHERE n.nspname = current_schema()
         AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
         AND NOT EXISTS (SELECT 1 FROM pg_depend dependency
           WHERE dependency.classid = 'pg_class'::regclass AND dependency.objid = c.oid
             AND dependency.deptype = 'e'))",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;

    if populated {
        verify_baseline_schema_on(connection).await?;
        if !casework_notifications_ready_on(connection).await? {
            let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
            transaction
                .execute(sqlx::raw_sql(CASEWORK_NOTIFICATIONS))
                .await
                .map_err(map_sqlx_error)?;
            if !casework_notifications_ready_on(&mut transaction).await? {
                return Err(DatabaseError::SchemaObjectsMissing);
            }
            transaction.commit().await.map_err(map_sqlx_error)?;
        }
        // Concurrent index builds must run outside the schema transaction.
        super::query_indexes::apply(connection).await?;
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
    verify_schema_compatibility_on(&mut transaction).await?;
    transaction.commit().await.map_err(map_sqlx_error)?;
    Ok(1)
}
