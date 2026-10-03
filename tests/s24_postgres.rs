#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use futures_util::StreamExt as _;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::privacy::export::DataExportService;
use histae_api_rust::privacy::export_pg::PgDataExportStore;
use histae_api_rust::privacy::rights::{
    DataRequestStatus, DataRequestTransition, DataRequestType, DataRightsStore, UpdateRequestInput,
    UpdateRequestResult,
};
use histae_api_rust::privacy::rights_pg::PgDataRightsStore;
use histae_api_rust::profiles::service::{ProfilePhotoUrlFuture, ProfilePhotoUrlProvider};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S24 fixture error ({})", self.0)
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

fn local_config() -> Result<PostgresConfig, FixtureError> {
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
        port: env::var("POSTGRES_PORT")
            .unwrap_or_else(|_| "5432".to_owned())
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: env::var("POSTGRES_SSLMODE").is_ok_and(|value| value != "disable"),
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s24-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn insert_account(
    database: &Database,
    user_id: Uuid,
    role: &str,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(role)
    .bind(format!("s24-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, users: &[Uuid]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM data_access_log WHERE accessed_user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[derive(Clone)]
struct Photos;

impl ProfilePhotoUrlProvider for Photos {
    fn url_for_key(&self, _object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn dsr_and_export_preserve_snapshot_privacy_audit_and_erasure_scheduling()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let user_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();
    let admin_id = Uuid::new_v4();
    let users = [user_id, other_id, admin_id];
    cleanup(&database, &users).await?;
    insert_account(&database, user_id, "user").await?;
    insert_account(&database, other_id, "user").await?;
    insert_account(&database, admin_id, "admin").await?;

    let mut connection = database.acquire().await?;
    let first_swipe = Utc::now() - TimeDelta::seconds(2);
    let second_swipe = Utc::now() - TimeDelta::seconds(1);
    sqlx::query(
        "INSERT INTO swipe_decision
           (actor_id, target_id, decision, swiped_at, expires_at)
         VALUES ($1, $2, 'like', $3, $3 + INTERVAL '365 days')",
    )
    .bind(user_id)
    .bind(other_id)
    .bind(first_swipe)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO swipe_decision
           (actor_id, target_id, decision, swiped_at, expires_at)
         VALUES ($1, $2, 'pass', $3, $3 + INTERVAL '365 days')",
    )
    .bind(other_id)
    .bind(user_id)
    .bind(second_swipe)
    .execute(&mut *connection)
    .await?;
    drop(connection);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let rights = Arc::new(PgDataRightsStore::new(database.clone()));
        let access = rights
            .create_request(user_id, DataRequestType::Access)
            .await?
            .ok_or(FixtureError("create access DSR"))?;
        assert!(
            rights
                .create_request(user_id, DataRequestType::Access)
                .await?
                .is_none()
        );
        assert_eq!(rights.requests_for_user(user_id).await?.len(), 1);
        assert_eq!(
            rights
                .update_request(UpdateRequestInput {
                    request_id: access.id,
                    status: DataRequestTransition::InProgress,
                    admin_id,
                    admin_role: AdminRole::Admin,
                    notes: Some("Identity verified".to_owned()),
                })
                .await?,
            UpdateRequestResult::Updated
        );
        assert_eq!(
            rights
                .update_request(UpdateRequestInput {
                    request_id: access.id,
                    status: DataRequestTransition::Completed,
                    admin_id,
                    admin_role: AdminRole::Admin,
                    notes: Some("Export supplied".to_owned()),
                })
                .await?,
            UpdateRequestResult::Updated
        );

        let export = DataExportService::new(
            Arc::new(PgDataExportStore::new(database.clone())),
            rights.clone(),
            Arc::new(Photos),
            1,
            8 * 1024 * 1024,
            1,
        );
        let mut prepared = export.prepare(user_id).await.map_err(|error| {
            let label = match error {
                histae_api_rust::privacy::export::DataExportError::Busy => "export busy",
                histae_api_rust::privacy::export::DataExportError::TooLarge => "export too large",
                histae_api_rust::privacy::export::DataExportError::Unavailable => {
                    "export unavailable"
                }
            };
            FixtureError(label)
        })?;
        let mut bytes = Vec::new();
        while let Some(chunk) = prepared.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        let document: Value = serde_json::from_slice(&bytes)?;
        let outgoing = document["discovery_actions"]["outgoing"]
            .as_array()
            .ok_or(FixtureError("outgoing swipes"))?;
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0]["actor_id"], user_id.to_string());
        assert_eq!(outgoing[0]["target_id"], other_id.to_string());
        assert_eq!(
            document["consistency"]["postgres"]["level"],
            "repeatable_read"
        );
        assert_eq!(document["consistency"]["discovery"]["rows"], 1);
        assert!(document.get("incoming").is_none());

        let erasure = rights
            .create_request(user_id, DataRequestType::Erasure)
            .await?
            .ok_or(FixtureError("create erasure DSR"))?;
        assert_eq!(
            rights
                .update_request(UpdateRequestInput {
                    request_id: erasure.id,
                    status: DataRequestTransition::InProgress,
                    admin_id,
                    admin_role: AdminRole::Admin,
                    notes: None,
                })
                .await?,
            UpdateRequestResult::Updated
        );
        let schedule = || UpdateRequestInput {
            request_id: erasure.id,
            status: DataRequestTransition::Completed,
            admin_id,
            admin_role: AdminRole::Admin,
            notes: Some("Identity verified".to_owned()),
        };
        assert_eq!(
            rights.update_request(schedule()).await?,
            UpdateRequestResult::ErasureScheduled
        );
        assert_eq!(
            rights.update_request(schedule()).await?,
            UpdateRequestResult::ErasureScheduled
        );

        let mut connection = database.acquire().await?;
        let request_status: String =
            sqlx::query_scalar("SELECT status FROM data_subject_request WHERE id = $1")
                .bind(erasure.id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(request_status, DataRequestStatus::InProgress.as_str());
        let deleted: bool = sqlx::query_scalar(
            "SELECT deleted_at IS NOT NULL FROM user_account WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(deleted);
        let workflow_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM account_erasure WHERE request_id = $1")
                .bind(erasure.id)
                .fetch_one(&mut *connection)
                .await?;
        let outbox_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox_event
             WHERE event_type = 'account.erase' AND aggregate_id = $1",
        )
        .bind(erasure.id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(workflow_count, 1);
        assert_eq!(outbox_count, 1);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log
             WHERE accessed_user_id = $1
               AND action IN ('export_data', 'admin_review_dsr')",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(audit_count, 5);
        Ok(())
    }
    .await;

    cleanup(&database, &users).await?;
    result
}
