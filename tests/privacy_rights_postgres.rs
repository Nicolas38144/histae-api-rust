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
use histae_api_rust::privacy::export::pg::PgDataExportStore;
use histae_api_rust::privacy::rights::pg::PgDataRightsStore;
use histae_api_rust::privacy::rights::{
    DataRequestStatus, DataRequestTransition, DataRequestType, DataRightsError, DataRightsService,
    DataRightsStore, UpdateRequestInput, UpdateRequestResult,
};
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
    sqlx::query("DELETE FROM billing_invoice WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
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
    sqlx::query("UPDATE user_account SET discovery_paused=true WHERE user_id=$1")
        .bind(user_id)
        .execute(&mut *connection)
        .await?;
    let first_swipe = Utc::now() - TimeDelta::seconds(2);
    let second_swipe = Utc::now() - TimeDelta::seconds(1);
    sqlx::query(
        "INSERT INTO swipe_decision
           (actor_id, target_id, decision, swiped_at, expires_at)
         VALUES ($1, $2, 'like', $3, $3 + INTERVAL '30 days')",
    )
    .bind(user_id)
    .bind(other_id)
    .bind(first_swipe)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO swipe_decision
           (actor_id, target_id, decision, swiped_at, expires_at)
         VALUES ($1, $2, 'pass', $3, $3 + INTERVAL '30 days')",
    )
    .bind(other_id)
    .bind(user_id)
    .bind(second_swipe)
    .execute(&mut *connection)
    .await?;
    drop(connection);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut fixture = database.acquire().await?;
        sqlx::query("INSERT INTO user_profile (user_id, firstname, birthdate) VALUES ($1, 'Fixture', '1990-02-03')")
            .bind(user_id).execute(&mut *fixture).await?;
        sqlx::query("INSERT INTO billing_invoice (stripe_invoice_id,user_id,stripe_customer_id,currency,amount_due,amount_paid,amount_remaining,period_starts_at,period_ends_at,created_at) VALUES ($1,$2,$3,'EUR',9007199254740993,0,9007199254740993,now(),now()+interval '1 month',now())")
            .bind(format!("in_{}", Uuid::new_v4().simple())).bind(user_id).bind(format!("cus_{}",Uuid::new_v4().simple()))
            .execute(&mut *fixture).await?;
        drop(fixture);
        let rights = Arc::new(PgDataRightsStore::new(database.clone()));
        let access = rights
            .create_request(user_id, DataRequestType::Access, None)
            .await?
            .ok_or(FixtureError("create access DSR"))?;
        assert!(
            rights
                .create_request(user_id, DataRequestType::Access, None)
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
                    response: None,
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
                    response: None,
                    notes: Some("Export supplied".to_owned()),
                })
                .await?,
            UpdateRequestResult::Updated
        );

        let details = "Correct the information I cannot edit in my profile.";
        let correction = rights
            .create_request(user_id, DataRequestType::Rectification, Some(details.to_owned()))
            .await?
            .ok_or(FixtureError("create rectification DSR"))?;
        assert_eq!(correction.details.as_deref(), Some(details));
        let owner_requests = rights.requests_for_user(user_id).await?;
        assert_eq!(
            owner_requests.iter().find(|row| row.id == correction.id)
                .and_then(|row| row.details.as_deref()),
            Some(details),
        );
        let admin_requests = rights.requests_for_admin(None, 100, 0, None).await?;
        assert_eq!(
            admin_requests.iter().find(|row| row.request.id == correction.id)
                .and_then(|row| row.request.details.as_deref()),
            Some(details),
        );

        let correction_update = |status, response| UpdateRequestInput {
            request_id: correction.id,
            status,
            admin_id,
            admin_role: AdminRole::Admin,
            notes: Some("Internal handling note".to_owned()),
            response,
        };
        assert_eq!(
            rights.update_request(correction_update(DataRequestTransition::InProgress, None)).await?,
            UpdateRequestResult::Updated,
        );
        assert_eq!(
            rights.update_request(correction_update(DataRequestTransition::Completed, None)).await?,
            UpdateRequestResult::InvalidRequest,
        );
        let response = "The historical information has been corrected.";
        assert_eq!(
            rights.update_request(correction_update(
                DataRequestTransition::Completed, Some(response.to_owned()),
            )).await?,
            UpdateRequestResult::Updated,
        );
        let resolved = rights.requests_for_user(user_id).await?;
        assert_eq!(
            resolved.iter().find(|row| row.id == correction.id)
                .and_then(|row| row.response.as_deref()),
            Some(response),
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
        assert_eq!(document["account"]["discovery_paused"], true);
        assert_eq!(document["profile"]["birthdate"], "1990-02-03T00:00:00.000Z");
        assert_eq!(document["billing_invoices"][0]["amount_due"], "9007199254740993");
        assert_eq!(document["billing_invoices"][0]["amount_paid"], "0");
        let exported_requests = document["data_requests"]
            .as_array()
            .ok_or(FixtureError("data requests"))?;
        let exported_correction = exported_requests
            .iter()
            .find(|row| row["id"] == correction.id.to_string())
            .ok_or(FixtureError("exported correction"))?;
        assert_eq!(exported_correction["details"], details);
        assert_eq!(exported_correction["response"], response);
        assert!(exported_requests.iter().all(|row| row.get("notes").is_none()));
        assert!(!String::from_utf8_lossy(&bytes).contains("Internal handling note"));
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
            .create_request(user_id, DataRequestType::Erasure, None)
            .await?
            .ok_or(FixtureError("create erasure DSR"))?;
        assert_eq!(
            rights
                .update_request(UpdateRequestInput {
                    request_id: erasure.id,
                    status: DataRequestTransition::InProgress,
                    admin_id,
                    admin_role: AdminRole::Admin,
                    response: None,
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
            response: None,
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
        assert_eq!(audit_count, 7);
        Ok(())
    }
    .await;

    cleanup(&database, &users).await?;
    result
}

#[tokio::test]
async fn correction_rejections_require_public_replies_and_retired_requests_stay_read_only()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let user_id = Uuid::new_v4();
    let admin_id = Uuid::new_v4();
    let users = [user_id, admin_id];
    insert_account(&database, user_id, "user").await?;
    insert_account(&database, admin_id, "admin").await?;
    let result = async {
        let store = Arc::new(PgDataRightsStore::new(database.clone()));
        let service = DataRightsService::new(store.clone());
        let correction = service
            .create_request(
                user_id,
                DataRequestType::Rectification,
                Some("  Incorrect birthdate  ".to_owned()),
            )
            .await
            .map_err(|_| FixtureError("create correction"))?;
        let update = |request_id, status, response: Option<&str>| UpdateRequestInput {
            request_id,
            status,
            admin_id,
            admin_role: AdminRole::Admin,
            notes: Some("Internal investigation".to_owned()),
            response: response.map(str::to_owned),
        };
        let rejection = |response| update(correction.id, DataRequestTransition::Rejected, response);
        for response in [None, Some(" \n\t"), Some("bad\0reply")] {
            assert_eq!(
                service.update_request(rejection(response)).await,
                Err(DataRightsError::InvalidRequest),
            );
        }
        // The locked-row policy also rejects blank replies from direct store callers.
        assert_eq!(
            store.update_request(rejection(Some(" \n\t"))).await?,
            UpdateRequestResult::InvalidRequest,
        );
        let pending = store.requests_for_user(user_id).await?;
        assert_eq!(pending[0].status, DataRequestStatus::Pending);
        assert_eq!(pending[0].response, None);
        let mut connection = database.acquire().await?;
        let audits: i64 =
            sqlx::query_scalar("SELECT count(*) FROM data_access_log WHERE accessed_user_id=$1")
                .bind(user_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(audits, 0);
        drop(connection);

        let reply = "  Please supply supporting documentation.\n";
        let (first, second) = tokio::join!(
            service.update_request(rejection(Some(reply))),
            service.update_request(rejection(Some(reply))),
        );
        assert!(matches!(
            (first, second),
            (
                Ok(UpdateRequestResult::Updated),
                Err(DataRightsError::InvalidTransition)
            ) | (
                Err(DataRightsError::InvalidTransition),
                Ok(UpdateRequestResult::Updated)
            )
        ));
        let rejected = store.requests_for_user(user_id).await?;
        assert_eq!(rejected[0].status, DataRequestStatus::Rejected);
        assert_eq!(rejected[0].details.as_deref(), Some("Incorrect birthdate"));
        assert_eq!(
            rejected[0].response.as_deref(),
            Some("Please supply supporting documentation."),
        );
        assert!(store.requests_for_user(admin_id).await?.is_empty());

        // Old categories are kept verbatim for history, never silently reclassified.
        let mut connection = database.acquire().await?;
        for retired in [DataRequestType::Restriction, DataRequestType::Objection] {
            let request_id: Uuid = sqlx::query_scalar(
                "INSERT INTO data_subject_request(user_id,type,notes)
                 VALUES ($1,$2,'Historical internal note') RETURNING id",
            )
            .bind(user_id)
            .bind(retired.as_str())
            .fetch_one(&mut *connection)
            .await?;
            assert_eq!(
                service
                    .update_request(update(request_id, DataRequestTransition::InProgress, None))
                    .await,
                Err(DataRightsError::InvalidTransition),
            );
            assert_eq!(
                service
                    .create_request(user_id, retired, Some("Message".to_owned()))
                    .await,
                Err(DataRightsError::InvalidRequest),
            );
        }
        let owner_history = service
            .requests_for_user(user_id)
            .await
            .map_err(|_| FixtureError("owner history"))?;
        for request in owner_history
            .iter()
            .filter(|request| request.request_type.is_retired())
        {
            assert_eq!(request.status, DataRequestStatus::Pending);
            assert!(request.details.is_none() && request.response.is_none());
        }
        assert_eq!(owner_history.len(), 3);
        let admin_history = service
            .requests_for_admin(None, 100, 0, None)
            .await
            .map_err(|_| FixtureError("admin history"))?;
        let reviewed = admin_history
            .items
            .iter()
            .find(|request| request.request.id == correction.id)
            .ok_or(FixtureError("admin correction"))?;
        assert_eq!(reviewed.notes.as_deref(), Some("Internal investigation"));
        assert_eq!(reviewed.request, rejected[0].clone().into());
        let audits: i64 =
            sqlx::query_scalar("SELECT count(*) FROM data_access_log WHERE accessed_user_id=$1")
                .bind(user_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(audits, 1);
        Ok(())
    }
    .await;
    cleanup(&database, &users).await?;
    result
}
