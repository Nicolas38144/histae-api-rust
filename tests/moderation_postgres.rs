#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::administration::photos::{
    AdminPhotoStore, PgAdminPhotoRepository, ReconciliationResult,
};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::moderation::domain::{
    ModerationDecision, ModerationReviewInput, ModerationReviewResult, PhotoReviewChecks,
};
use histae_api_rust::moderation::pg::{ModerationStore, PgModerationRepository};
use histae_api_rust::outbox::pg::PgOutboxRepository;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S16 integration fixture error ({})", self.0)
    }
}

impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
    let _ = dotenvy::dotenv();
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(FixtureError(name))
}

fn postgres_config() -> Result<PostgresConfig, FixtureError> {
    if variable("ENV")? != "development" {
        return Err(FixtureError("ENV"));
    }
    let host = variable("POSTGRES_HOST")?;
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(FixtureError("POSTGRES_HOST"));
    }
    let database = variable("POSTGRES_DB")?;
    if database != "histae-dev" {
        return Err(FixtureError("POSTGRES_DB"));
    }
    Ok(PostgresConfig {
        host,
        port: variable("POSTGRES_PORT")?
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: false,
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s16-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

async fn account(database: &Database, user_id: Uuid, role: &str) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(role)
    .bind(format!("s16-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(database.acquire().await?.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, ids: &[Uuid]) -> Result<(), DatabaseError> {
    sqlx::query("DELETE FROM outbox_event WHERE aggregate_id = ANY($1::uuid[])")
        .bind(ids)
        .execute(database.acquire().await?.as_mut())
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query(
        "DELETE FROM data_access_log
         WHERE accessed_user_id = ANY($1::uuid[]) OR accessor_id = ANY($1::uuid[])",
    )
    .bind(ids)
    .execute(database.acquire().await?.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(ids)
        .execute(database.acquire().await?.as_mut())
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn review_and_reconciliation_preserve_locks_audits_and_outbox_atomicity()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let outbox = PgOutboxRepository::new(database.clone());
    let moderation = PgModerationRepository::new(database.clone(), outbox.clone());
    let admin_photos = PgAdminPhotoRepository::new(database.clone(), outbox);
    let user_id = Uuid::new_v4();
    let admin_id = Uuid::new_v4();
    let ready_photo = Uuid::new_v4();
    let case_id = Uuid::new_v4();
    let stale_photo = Uuid::new_v4();
    let active_photo = Uuid::new_v4();
    let ids = [
        user_id,
        admin_id,
        ready_photo,
        case_id,
        stale_photo,
        active_photo,
    ];
    cleanup(&database, &ids).await?;
    account(&database, user_id, "user").await?;
    account(&database, admin_id, "admin").await?;
    sqlx::query(
        "INSERT INTO user_profile (user_id, firstname, birthdate)
         VALUES ($1, 'Alice', '1990-01-01')",
    )
    .bind(user_id)
    .execute(database.acquire().await?.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO user_photo
         (id, user_id, object_key, status, mime_type, size_bytes, width, height, sha256)
         VALUES ($1, $2, $3, 'ready', 'image/webp', 128, 64, 64, $4)",
    )
    .bind(ready_photo)
    .bind(user_id)
    .bind(format!("profile-photos/{user_id}/{ready_photo}.webp"))
    .bind(vec![7_u8; 32])
    .execute(database.acquire().await?.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO content_moderation_case
         (id, user_id, content_type, photo_id, status, reason_codes, policy_version,
          face_count, sharpness_score, nsfw_score)
         VALUES ($1, $2, 'photo', $3, 'pending', ARRAY['blurry']::text[],
                 'local_vision_v1', 1, 40, 0.1)",
    )
    .bind(case_id)
    .bind(user_id)
    .bind(ready_photo)
    .execute(database.acquire().await?.as_mut())
    .await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert!(
            moderation
                .detail(
                    case_id,
                    admin_id,
                    AdminRole::Admin,
                    "Contrôle manuel".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("moderation_detail"))?
                .is_some()
        );
        let view_audit: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log
             WHERE accessed_user_id = $1 AND accessor_id = $2
               AND action = 'view_moderation_content'",
        )
        .bind(user_id)
        .bind(admin_id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(view_audit, 1);

        let stale = moderation
            .review(
                case_id,
                ModerationReviewInput {
                    version: 2,
                    decision: ModerationDecision::Rejected,
                    reason: "Photo floue".to_owned(),
                    photo_checks: Some(PhotoReviewChecks {
                        face_detectable: true,
                        sharp_enough: false,
                        content_allowed: true,
                    }),
                },
                admin_id,
                AdminRole::Admin,
            )
            .await
            .map_err(|_| FixtureError("stale_review"))?;
        assert_eq!(stale, ModerationReviewResult::Stale);

        let reviewed = moderation
            .review(
                case_id,
                ModerationReviewInput {
                    version: 1,
                    decision: ModerationDecision::Rejected,
                    reason: "Photo floue".to_owned(),
                    photo_checks: Some(PhotoReviewChecks {
                        face_detectable: true,
                        sharp_enough: false,
                        content_allowed: true,
                    }),
                },
                admin_id,
                AdminRole::Admin,
            )
            .await
            .map_err(|_| FixtureError("photo_review"))?;
        assert_eq!(reviewed, ModerationReviewResult::Updated);
        let state: (String, i32, String, String) = sqlx::query_as(
            "SELECT moderation.status, moderation.version, photo.status, event.status
             FROM content_moderation_case AS moderation
             JOIN user_photo AS photo ON photo.id = moderation.photo_id
             JOIN outbox_event AS event ON event.aggregate_id = photo.id
                AND event.event_type = 'photo.delete'
             WHERE moderation.id = $1",
        )
        .bind(case_id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(
            state,
            (
                "rejected".to_owned(),
                2,
                "deleting".to_owned(),
                "pending".to_owned()
            )
        );

        let old = Utc::now() - TimeDelta::hours(1);
        sqlx::query(
            "INSERT INTO user_photo (id, user_id, object_key, status, updated_at)
             VALUES ($1, $2, $3, 'processing', $4)",
        )
        .bind(stale_photo)
        .bind(user_id)
        .bind(format!("profile-photos/{user_id}/{stale_photo}.webp"))
        .bind(old)
        .execute(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(
            admin_photos
                .reconcile(
                    stale_photo,
                    Utc::now() - TimeDelta::minutes(30),
                    Utc::now() - TimeDelta::minutes(5),
                    admin_id,
                    AdminRole::Admin,
                    "Traitement ancien".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("stale_reconcile"))?,
            ReconciliationResult::Queued
        );
        sqlx::query(
            "INSERT INTO user_photo (id, user_id, object_key, status, updated_at)
             VALUES ($1, $2, $3, 'processing', $4)",
        )
        .bind(active_photo)
        .bind(user_id)
        .bind(format!("profile-photos/{user_id}/{active_photo}.webp"))
        .bind(old)
        .execute(database.acquire().await?.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO outbox_event
             (id, event_type, aggregate_id, status, locked_at, locked_by)
             VALUES ($1, 'photo.delete', $2, 'processing', clock_timestamp(), $3)",
        )
        .bind(Uuid::new_v4())
        .bind(active_photo)
        .bind(Uuid::new_v4())
        .execute(database.acquire().await?.as_mut())
        .await?;
        sqlx::query("UPDATE user_photo SET status = 'deleting' WHERE id = $1")
            .bind(active_photo)
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            admin_photos
                .reconcile(
                    active_photo,
                    Utc::now() - TimeDelta::minutes(30),
                    Utc::now() - TimeDelta::minutes(5),
                    admin_id,
                    AdminRole::Admin,
                    "Worker actif".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("active_reconcile"))?,
            ReconciliationResult::AlreadyProcessing
        );
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &ids).await;
    database.close().await;
    result?;
    cleanup_result?;
    Ok(())
}
