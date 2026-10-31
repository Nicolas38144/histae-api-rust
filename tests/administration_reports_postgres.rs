#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::administration::domain::{BanResult, PageCursor};
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

async fn message_audit_count(database: &Database, actor: Uuid) -> Result<i64, DatabaseError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM data_access_log WHERE accessor_id=$1 AND action='view_messages'",
    )
    .bind(actor)
    .fetch_one(&mut *database.acquire().await?)
    .await
    .map_err(map_sqlx_error)
}

// Real messages ensure the adapter cannot return another conversation or leak a
// later page after report eligibility changes. Both actor roles follow this rule.
#[tokio::test]
async fn administrative_messages_require_a_report_in_the_exact_conversation()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let admin = Uuid::new_v4();
    let superadmin = Uuid::new_v4();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let users = [admin, superadmin, first, second, third];
    let actors = [
        (admin, AdminRole::Admin),
        (superadmin, AdminRole::Superadmin),
    ];
    for (actor, role) in actors {
        insert_account(&database, actor, role.as_str()).await?;
    }
    for user in [first, second, third] {
        insert_account(&database, user, "user").await?;
    }
    let conversation = Uuid::new_v4();
    let unrelated = Uuid::new_v4();
    let missing = Uuid::new_v4();
    let report_id = Uuid::new_v4();
    let latest = Uuid::new_v4();
    let older = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let created_at = Utc::now() - TimeDelta::minutes(2);
        for (id, left, right) in [(conversation, first, second), (unrelated, second, third)] {
            sqlx::query(
                "INSERT INTO match_init (id,user1_id,user2_id,status,expires_at,created_at)
                 VALUES ($1,$2,$3,'confirmed',clock_timestamp()+interval '1 day',$4)",
            )
            .bind(id).bind(left.min(right)).bind(left.max(right)).bind(created_at)
            .execute(&mut *database.acquire().await?).await?;
        }
        for (id, thread, sender, content, seconds) in [
            (older, conversation, first, "Earlier conversation message", 10),
            (latest, conversation, second, "Latest conversation message", 20),
            (foreign, unrelated, third, "Message from another conversation", 30),
        ] {
            sqlx::query("INSERT INTO chat_message(id,match_id,sender_id,content,created_at) VALUES($1,$2,$3,$4,$5)")
                .bind(id).bind(thread).bind(sender).bind(content).bind(created_at + TimeDelta::seconds(seconds))
                .execute(&mut *database.acquire().await?).await?;
        }
        let cursor_at = sqlx::query_scalar("SELECT created_at FROM chat_message WHERE id=$1")
            .bind(latest).fetch_one(&mut *database.acquire().await?).await?;
        let cursor = PageCursor { at: cursor_at, id: latest };
        let store = PgAdministrationStore::new(database.clone());
        let reports = PgReportStore::new(database.clone());
        for (context, reporter, reported, status, allowed) in [
            (None, first, second, ReportStatus::Pending, false),
            (Some(unrelated), third, second, ReportStatus::Pending, false),
            // A legacy inconsistent report must not authorize either orientation.
            (Some(conversation), first, third, ReportStatus::Pending, false),
            (Some(conversation), third, first, ReportStatus::Pending, false),
            (Some(conversation), first, second, ReportStatus::Pending, true),
            (Some(conversation), second, first, ReportStatus::Reviewed, true),
            (Some(conversation), first, second, ReportStatus::Dismissed, true),
        ] {
            sqlx::query("DELETE FROM user_report WHERE id=$1").bind(report_id)
                .execute(&mut *database.acquire().await?).await?;
            reports.create(ReportRecord {
                id: report_id, reporter_id: reporter, reported_id: reported, match_id: context,
                reason: ReportReason::Harassment, description: None, status,
                created_at: Utc::now(), resolved_at: None,
            }).await?;
            for (actor, role) in actors {
                let before = message_audit_count(&database, actor).await?;
                let rows = store.messages(conversation, actor, role, "Report review".to_owned(), 1, 0, None).await?;
                assert_eq!(rows.is_some(), allowed);
                if let Some(rows) = rows {
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0].item.id, latest);
                    assert_eq!(rows[0].item.content, "Latest conversation message");
                }
                let later = store.messages(conversation, actor, role, "Report review".to_owned(), 1, 1, None).await?;
                assert_eq!(later.is_some(), allowed);
                if let Some(rows) = later { assert_eq!(rows[0].item.id, older); }
                let cursor_page = store.messages(conversation, actor, role, "Report review".to_owned(), 1, 0, Some(cursor.clone())).await?;
                assert_eq!(cursor_page.is_some(), allowed);
                if let Some(rows) = cursor_page { assert_eq!(rows[0].item.id, older); }
                let listed = store.matches(first, actor, role, "Profile review".to_owned(), 10, 0, None).await?.expect("account");
                let listed = listed.iter().find(|row| row.item.id == conversation).expect("match");
                assert_eq!(listed.can_view_messages, allowed);
                let other = store.messages(unrelated, actor, role, "Report review".to_owned(), 1, 0, None).await?;
                assert_eq!(other.is_some(), context == Some(unrelated));
                if let Some(rows) = other { assert_eq!(rows[0].item.id, foreign); }
                assert!(store.messages(missing, actor, role, "Report review".to_owned(), 1, 0, None).await?.is_none());
                if allowed {
                    // No message may be returned if writing its audit fails.
                    assert!(store.messages(conversation, actor, role, "Invalid\0audit".to_owned(), 1, 0, None).await.is_err());
                }
                let after = message_audit_count(&database, actor).await?;
                let authorized_reads = if allowed { 3 } else if context == Some(unrelated) { 1 } else { 0 };
                assert_eq!(after - before, authorized_reads * 2);
            }
        }
        // A cursor obtained while authorized never bypasses a later revocation.
        sqlx::query("DELETE FROM user_report WHERE id=$1").bind(report_id)
            .execute(&mut *database.acquire().await?).await?;
        for (actor, role) in actors {
            let before = message_audit_count(&database, actor).await?;
            for page in [None, Some(cursor.clone())] {
                assert!(store.messages(conversation, actor, role, "Report review".to_owned(), 1, 0, page).await?.is_none());
            }
            assert_eq!(message_audit_count(&database, actor).await?, before);
            let listed = store.matches(first, actor, role, "Profile review".to_owned(), 10, 0, None).await?.expect("account");
            assert!(!listed.iter().find(|row| row.item.id == conversation).expect("match").can_view_messages);
        }
        Ok(())
    }.await;
    cleanup(&database, &users).await?;
    database.close().await;
    result
}
