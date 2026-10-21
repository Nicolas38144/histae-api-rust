#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::administration::domain::{AdminUserRole, AdminUserStatus, PageCursor};
use histae_api_rust::administration::pg::PgAdministrationStore;
use histae_api_rust::administration::store::AdministrationStore;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL query fixture error ({})", self.0)
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
        application_name: "histae-rust-query-integration",
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
    .bind(format!("query-test-{user_id}"))
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
async fn admin_pages_and_detail_preserve_filters_counts_and_optional_data()
-> Result<(), Box<dyn std::error::Error>> {
    let config = local_config()?;
    let database = Database::connect(&config).await?;
    let mut users = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    users.sort();
    let [first, middle, last, admin] = users;
    let prefix = format!("Query{}", Uuid::new_v4().simple());
    for user in users {
        insert_account(
            &database,
            user,
            if user == admin { "admin" } else { "user" },
        )
        .await?;
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        for user in [first, middle, last] {
            sqlx::query("INSERT INTO user_profile(user_id, firstname, birthdate, sex)
                VALUES ($1, $2, '1995-01-01', 'male')")
                .bind(user).bind(format!("{prefix} {user}")).execute(&mut *connection).await?;
        }
        let at = Utc::now() - TimeDelta::seconds(5);
        sqlx::query("UPDATE user_account SET created_at = $2, is_banned = (user_id = $3)
            WHERE user_id = ANY($1)").bind(&users[..]).bind(at).bind(middle).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO match_init(user1_id, user2_id) VALUES ($1, $2), ($2, $3)")
            .bind(first).bind(middle).bind(last).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO user_report(reporter_id,reported_id,reason) VALUES ($1,$2,'other')")
            .bind(first).bind(middle).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO user_preferences(user_id,min_age,max_age,max_distance_km,looking_for)
            VALUES ($1,20,40,25,'both')").bind(middle).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO user_presence(user_id,is_location_fresh,updated_at) VALUES ($1,false,$2)")
            .bind(middle).bind(at).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO user_consent(user_id,consent_type,granted,document_version,granted_at,withdrawn_at)
            VALUES ($1,'location_consent',true,'old',$2,$2), ($1,'location_consent',false,'new',$2,NULL)")
            .bind(middle).bind(at).execute(&mut *connection).await?;
        drop(connection);
        let store = PgAdministrationStore::new(database.clone());
        let page = store.list_users(None,None,prefix.clone(),2,0,None,"v1".into(),"v1".into()).await?;
        assert_eq!(page.iter().map(|row| row.id).collect::<Vec<_>>(), vec![last,middle]);
        assert_eq!(page[1].matches_count,2);
        assert_eq!(page[1].reports_received,1);
        let cursor = PageCursor { at, id: middle };
        let next = store.list_users(None,None,prefix.clone(),2,0,Some(cursor),"v1".into(),"v1".into()).await?;
        assert_eq!(next.iter().map(|row| row.id).collect::<Vec<_>>(), vec![first]);
        let offset = store.list_users(None,None,prefix.clone(),1,2,None,"v1".into(),"v1".into()).await?;
        assert_eq!(offset[0].id,first);
        for _ in 0..8 {
            // Exercise prepared statement reuse with both filtered and unfiltered pages.
            let banned = store.list_users(Some(AdminUserStatus::Banned),Some(AdminUserRole::User),prefix.clone(),10,0,None,"v1".into(),"v1".into()).await?;
            assert_eq!(banned.len(),1);
            assert_eq!(banned[0].id,middle);
        }
        let by_uuid = store.list_users(None,None,admin.to_string(),10,0,None,"v1".into(),"v1".into()).await?;
        assert_eq!(by_uuid.len(),1);
        assert_eq!(by_uuid[0].id,admin);
        assert!(by_uuid[0].firstname.is_none());
        let detail = store.user_detail(middle,admin,AdminRole::Admin,"Query optimization test".into(),"v1".into(),"v1".into()).await?.ok_or(FixtureError("detail"))?;
        assert_eq!(detail.user.matches_count,2);
        assert_eq!(detail.user.reports_received,1);
        assert_eq!(detail.preferences.ok_or(FixtureError("preferences"))?.max_distance_km,25);
        assert!(!detail.presence.ok_or(FixtureError("presence"))?.is_location_fresh);
        assert_eq!(detail.consents.len(),1);
        assert!(!detail.consents[0].granted);
        assert_eq!(detail.consents[0].document_version,"new");
        let absent_profile = store.user_detail(admin,admin,AdminRole::Admin,"Query optimization test".into(),"v1".into(),"v1".into()).await?.ok_or(FixtureError("detail_without_profile"))?;
        assert!(absent_profile.preferences.is_none());
        assert!(absent_profile.presence.is_none());
        assert!(absent_profile.traits.is_empty());
        assert!(absent_profile.consents.is_empty());
        let mut connection = database.acquire().await?;
        let logs: i64 = sqlx::query_scalar("SELECT count(*) FROM data_access_log WHERE accessed_user_id = ANY($1) AND accessor_id=$2")
            .bind(&users[..]).bind(admin).fetch_one(&mut *connection).await?;
        assert_eq!(logs,2);
        drop(connection);
        // Query checks use the limited runtime role. Schema upgrades are tested
        // separately with the migrator by verify-continuation-postgres.py.
        database.verify_schema_compatibility().await?;
        Ok(())
    }.await;
    let cleanup_result = cleanup(&database, &users).await;
    database.close().await;
    cleanup_result?;
    result
}

#[tokio::test]
async fn operational_queues_equal_independent_counts() -> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::identity::mobile::sweego::SweegoWebhookMetrics;
    use histae_api_rust::operations::status::{PersistentStatus, PgOperationalStatus};
    let database = Database::connect(&local_config()?).await?;
    let repository = PgOperationalStatus::new(
        database.clone(),
        std::sync::Arc::new(SweegoWebhookMetrics::default()),
        false,
        histae_api_rust::identity::mobile::sms_safety::SmsSafetyService::new(
            std::sync::Arc::new(
                histae_api_rust::identity::mobile::sms_safety::PgSmsSafetyStore::new(
                    database.clone(),
                ),
            ),
            Default::default(),
            SecretString::new("test-sms-key".into()),
            false,
        ),
    );
    let snapshot = repository.snapshot().await?;
    let mut connection = database.acquire().await?;
    for (condition, actual) in [
        ("true", snapshot.outbox),
        (
            "event_type = 'notification.push'",
            snapshot.notification_push,
        ),
        (
            "event_type LIKE 'billing.%'",
            snapshot.billing_reconciliation,
        ),
    ] {
        type Counts = (i64, i64, i64, i64, i64, Option<chrono::DateTime<Utc>>);
        let query = format!(
            "SELECT count(*) FILTER (WHERE status='pending')::bigint,
            count(*) FILTER (WHERE status='processing')::bigint,
            count(*) FILTER (WHERE status='completed')::bigint,
            count(*) FILTER (WHERE status='dead_letter')::bigint,
            count(*) FILTER (WHERE status='discarded')::bigint,
            min(available_at) FILTER (WHERE status='pending') FROM outbox_event WHERE {condition}"
        );
        let expected: Counts = sqlx::query_as(&query).fetch_one(&mut *connection).await?;
        assert_eq!(
            (
                actual.pending,
                actual.processing,
                actual.completed,
                actual.dead_letter,
                actual.discarded,
                actual.oldest_pending_at
            ),
            expected
        );
    }
    drop(connection);
    database.close().await;
    Ok(())
}
