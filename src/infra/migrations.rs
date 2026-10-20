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
         AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
         AND NOT EXISTS (SELECT 1 FROM pg_depend dependency
           WHERE dependency.classid = 'pg_class'::regclass AND dependency.objid = c.oid
             AND dependency.deptype = 'e'))",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;

    if populated {
        upgrade_existing_schema(connection).await?;
        sqlx::query("CREATE EXTENSION IF NOT EXISTS postgis WITH SCHEMA public")
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
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
    sqlx::query("ALTER TABLE user_account ADD COLUMN IF NOT EXISTS discovery_paused boolean NOT NULL DEFAULT false")
        .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    sqlx::raw_sql("ALTER TABLE user_account ADD COLUMN IF NOT EXISTS preferred_language text NOT NULL DEFAULT 'fr' CHECK (preferred_language IN ('fr','en','es','it')); ALTER TABLE device_token ADD COLUMN IF NOT EXISTS language text CHECK (language IN ('fr','en','es','it')); ALTER TABLE user_consent ADD COLUMN IF NOT EXISTS document_language text NOT NULL DEFAULT 'fr' CHECK (document_language IN ('fr','en','es','it')); ALTER TABLE otp_verification ADD COLUMN IF NOT EXISTS language text NOT NULL DEFAULT 'fr' CHECK (language IN ('fr','en','es','it'))")
        .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    let legacy_checkout: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'billing_checkout_session' AND column_name = 'stripe_checkout_request')")
        .fetch_one(&mut *transaction).await.map_err(map_sqlx_error)?;
    sqlx::query("ALTER TABLE billing_checkout_session ADD COLUMN IF NOT EXISTS stripe_checkout_request jsonb")
        .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    sqlx::query("ALTER TABLE billing_checkout_session ADD COLUMN IF NOT EXISTS creation_generation bigint NOT NULL DEFAULT 0")
        .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    if legacy_checkout {
        // Old attempts did not retain their POST parameters. Never replay a
        // potentially executed Checkout with newly reconstructed parameters.
        sqlx::query(r#"UPDATE billing_checkout_session AS checkout
                     SET status='failed', stripe_checkout_request='{"legacy_unrecoverable":true}'::jsonb
                     WHERE status IN ('creating','failed')
                       AND (created_customer_id IS NOT NULL OR EXISTS (
                         SELECT 1 FROM billing_customer AS customer
                         WHERE customer.user_id=checkout.user_id AND stripe_customer_deleted_at IS NULL))"#)
            .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    }
    let legacy_quota: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'continuation_usage' AND column_name = 'free_used_count')")
        .fetch_one(&mut *transaction).await.map_err(map_sqlx_error)?;
    sqlx::query("ALTER TABLE continuation_usage ADD COLUMN IF NOT EXISTS free_used_count smallint NOT NULL DEFAULT 0 CHECK (free_used_count >= 0)")
        .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    // Backfill only when upgrading the legacy quota schema. New advertising debits
    // must never become free debits on a repeated migration.
    if legacy_quota {
        sqlx::query("UPDATE continuation_usage SET free_used_count = used_count")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        // Legacy pending choices had no reservation and disclosed an initiator-only
        // cost. Ask for fresh agreement under the new per-participant contract.
        sqlx::query("UPDATE match_init SET continuation_initiator_id=NULL WHERE status='awaiting_continuation'")
            .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
        sqlx::query("UPDATE match_state SET continued = false WHERE match_id IN (SELECT id FROM match_init WHERE status = 'awaiting_continuation')")
            .execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    }
    let localization_new: bool =
        sqlx::query_scalar("SELECT to_regclass(current_schema() || '.trait_translation') IS NULL")
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
    for (source, name) in [
        (SCHEMA, "localization"),
        (SCHEMA, "continuation credits"),
        (SCHEMA, "continuation guards"),
        (SCHEMA, "account anonymization"),
        (SEED, "subscription plan features"),
        (SEED, "catalog translations"),
    ] {
        // Seed only at introduction. Later deploys must preserve deliberate removals too.
        if name == "catalog translations" && !localization_new {
            continue;
        }
        transaction
            .execute(sqlx::raw_sql(schema_section(source, name)?))
            .await
            .map_err(map_sqlx_error)?;
    }
    let reward_index: Option<String> = sqlx::query_scalar(
        "SELECT pg_get_indexdef(to_regclass(current_schema() || '.idx_advertising_reward_owner'))",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if reward_index
        .is_some_and(|definition| !definition.contains("(user_id, created_at DESC, id DESC)"))
    {
        sqlx::query("DROP INDEX idx_advertising_reward_owner")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query("CREATE INDEX idx_advertising_reward_owner ON advertising_reward_intent(user_id,created_at DESC,id DESC)").execute(&mut *transaction).await.map_err(map_sqlx_error)?;
    }
    // Remove the retired catalogue setting without changing historical subscriptions.
    sqlx::query("ALTER TABLE subscription_plan DROP COLUMN IF EXISTS trial_days")
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
    // Upgrade the former EUR catalogue price without overwriting custom prices.
    // Stripe Prices and existing subscriptions are managed separately.
    sqlx::query(
        "UPDATE subscription_plan
         SET annual_price_cents = 4800, updated_at = clock_timestamp()
         WHERE code = 'premium' AND currency = 'EUR' AND annual_price_cents = 3000",
    )
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
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

fn schema_section<'a>(source: &'a str, name: &str) -> Result<&'a str, DatabaseError> {
    source
        .split(&format!("-- BEGIN {name}"))
        .nth(1)
        .and_then(|section| section.split(&format!("-- END {name}")).next())
        .ok_or(DatabaseError::SchemaObjectsMissing)
}
