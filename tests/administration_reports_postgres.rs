#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::administration::domain::BanResult;
use histae_api_rust::administration::pg::PgAdministrationStore;
use histae_api_rust::administration::store::AdministrationStore;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{ConstraintKind, Database, DatabaseError, map_sqlx_error};
use histae_api_rust::privacy::pg::PgPrivacyStore;
use histae_api_rust::privacy::store::PrivacyStore;
use histae_api_rust::reports::domain::{ReportReason, ReportRecord, ReportStatus};
use histae_api_rust::reports::pg::PgReportStore;
use histae_api_rust::reports::store::ReportStore;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S23 fixture error ({})", self.0)
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
        application_name: "histae-rust-s23-integration",
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
    .bind(format!("s23-{user_id}"))
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

#[tokio::test]
async fn blocks_reports_bans_and_audits_preserve_postgres_invariants()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let admin_id = Uuid::new_v4();
    let reporter_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let users = [admin_id, reporter_id, target_id];
    cleanup(&database, &users).await?;
    insert_account(&database, admin_id, "admin").await?;
    insert_account(&database, reporter_id, "user").await?;
    insert_account(&database, target_id, "user").await?;

    let [user1, user2] = if reporter_id < target_id {
        [reporter_id, target_id]
    } else {
        [target_id, reporter_id]
    };
    let match_id = Uuid::new_v4();
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO match_init
         (id, user1_id, user2_id, status, expires_at, created_at)
         VALUES ($1, $2, $3, 'active', $4, clock_timestamp())",
    )
    .bind(match_id)
    .bind(user1)
    .bind(user2)
    .bind(Utc::now() + TimeDelta::days(1))
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO refresh_token_family
         (id, user_id, created_at, last_refreshed_at, expires_at)
         VALUES ($1, $2, clock_timestamp(), clock_timestamp(), $3)",
    )
    .bind(session_id)
    .bind(target_id)
    .bind(Utc::now() + TimeDelta::days(1))
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    drop(connection);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let privacy = PgPrivacyStore::new(database.clone());
        assert!(privacy.block(reporter_id, target_id).await?);
        let mut connection = database.acquire().await?;
        let match_status: String =
            sqlx::query_scalar("SELECT status FROM match_init WHERE id = $1")
                .bind(match_id)
                .fetch_one(&mut *connection)
                .await?;
        drop(connection);
        assert_eq!(match_status, "ended");
        assert_eq!(privacy.blocked_users(reporter_id).await?.len(), 1);

        let reports = PgReportStore::new(database.clone());
        let report = ReportRecord {
            id: Uuid::new_v4(),
            reporter_id,
            reported_id: target_id,
            match_id: Some(match_id),
            reason: ReportReason::Harassment,
            description: Some("Messages insistants.".to_owned()),
            status: ReportStatus::Pending,
            created_at: Utc::now(),
            resolved_at: None,
        };
        reports.create(report.clone()).await?;
        assert_eq!(
            reports
                .create(ReportRecord {
                    id: Uuid::new_v4(),
                    ..report
                })
                .await,
            Err(DatabaseError::Constraint(ConstraintKind::Unique))
        );

        let administration = PgAdministrationStore::new(database.clone());
        assert_eq!(
            administration
                .set_ban(
                    target_id,
                    true,
                    "Incident sécurité".to_owned(),
                    admin_id,
                    AdminRole::Admin,
                )
                .await?,
            BanResult::Updated
        );
        let mut connection = database.acquire().await?;
        let revoked: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM refresh_token_family WHERE id = $1")
                .bind(session_id)
                .fetch_one(&mut *connection)
                .await?;
        assert!(revoked.is_some());
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log
             WHERE accessed_user_id = $1 AND action = 'admin_ban'",
        )
        .bind(target_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(audit_count, 1);
        assert_eq!(
            administration
                .set_ban(
                    admin_id,
                    true,
                    "Self action".to_owned(),
                    admin_id,
                    AdminRole::Admin,
                )
                .await?,
            BanResult::Forbidden
        );
        Ok(())
    }
    .await;
    cleanup(&database, &users).await?;
    result
}
