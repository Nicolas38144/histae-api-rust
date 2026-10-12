//! Index upgrades for populated databases. Keep the empty-schema definitions in
//! db/001_schema_postgres.sql in sync; both paths are exercised against PostgreSQL.

use super::postgres::{DatabaseError, map_sqlx_error};

const INDEXES: &[(&str, &str)] = &[
    (
        "idx_user_presence_geography",
        "user_presence USING gist ((public.st_setsrid(public.st_makepoint(longitude::double precision, latitude::double precision), 4326)::public.geography)) WHERE is_location_fresh AND latitude BETWEEN -90 AND 90 AND longitude BETWEEN -180 AND 180",
    ),
    (
        "idx_outbox_event_claim",
        "outbox_event (available_at, created_at, id) WHERE status IN ('pending', 'processing')",
    ),
    (
        "idx_user_account_role_created",
        "user_account (role, created_at DESC, user_id DESC) WHERE deleted_at IS NULL",
    ),
    (
        "idx_user_account_banned_by",
        "user_account (banned_by) WHERE banned_by IS NOT NULL",
    ),
    ("idx_user_trait_trait_id", "user_trait (trait_id, user_id)"),
    ("idx_user_photo_user_id", "user_photo (user_id, id)"),
    (
        "idx_photo_upload_request_photo_id",
        "photo_upload_request (photo_id) WHERE photo_id IS NOT NULL",
    ),
    (
        "idx_content_moderation_reviewed_by",
        "content_moderation_case (reviewed_by) WHERE reviewed_by IS NOT NULL",
    ),
    (
        "idx_match_init_continuation_initiator",
        "match_init (continuation_initiator_id) WHERE continuation_initiator_id IS NOT NULL",
    ),
    (
        "idx_outbox_event_resolved_by",
        "outbox_event (resolved_by) WHERE resolved_by IS NOT NULL",
    ),
    (
        "idx_dsr_handled_by",
        "data_subject_request (handled_by) WHERE handled_by IS NOT NULL",
    ),
    (
        "idx_admin_credential_owner",
        "admin_webauthn_credential (user_id)",
    ),
    ("idx_admin_session_owner", "admin_session (user_id)"),
    (
        "idx_admin_session_credential",
        "admin_session (credential_id)",
    ),
    (
        "idx_admin_bootstrap_owner",
        "admin_webauthn_bootstrap (user_id)",
    ),
    (
        "idx_admin_challenge_owner",
        "admin_webauthn_challenge (user_id) WHERE user_id IS NOT NULL",
    ),
    (
        "idx_admin_challenge_bootstrap",
        "admin_webauthn_challenge (bootstrap_id) WHERE bootstrap_id IS NOT NULL",
    ),
    (
        "idx_admin_auth_event_credential",
        "admin_auth_event (credential_id) WHERE credential_id IS NOT NULL",
    ),
    (
        "idx_admin_auth_event_session",
        "admin_auth_event (session_id) WHERE session_id IS NOT NULL",
    ),
];

pub(super) async fn apply(connection: &mut sqlx::PgConnection) -> Result<(), DatabaseError> {
    // This is the migrator's dedicated connection, closed by its caller. Long
    // builds are allowed, while waiting for conflicting DDL remains bounded.
    sqlx::query("SELECT set_config('statement_timeout', '0', false), set_config('lock_timeout', '5s', false)")
        .execute(&mut *connection).await.map_err(map_sqlx_error)?;
    for &(name, definition) in INDEXES {
        let valid: Option<bool> = sqlx::query_scalar(
            "SELECT index.indisvalid FROM pg_index index
             JOIN pg_class relation ON relation.oid = index.indexrelid
             JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
             WHERE namespace.nspname = current_schema() AND relation.relname = $1",
        )
        .bind(name)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        if valid == Some(true) {
            continue;
        }
        // An interrupted concurrent build can leave an invalid index behind.
        // IF NOT EXISTS alone would skip it forever; retry must repair it.
        if valid == Some(false) {
            sqlx::query(&format!("DROP INDEX CONCURRENTLY {name}"))
                .execute(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
        }
        sqlx::query(&format!(
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {definition}"
        ))
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    Ok(())
}
