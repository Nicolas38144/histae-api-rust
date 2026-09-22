#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::Database;
use histae_api_rust::infra::postgres_locks::{
    AccountActivityError, AccountActivityPool, MATCH_MAINTENANCE_LOCK, SessionLeaderLease,
    TryExclusive,
};
use sqlx::Acquire;
use tokio::sync::{Notify, oneshot};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const FIRST_ACCOUNT: Uuid = Uuid::from_u128(0x7100_0000_0000_4000_8000_0000_0000_0001);
const SECOND_ACCOUNT: Uuid = Uuid::from_u128(0x7100_0000_0000_4000_8000_0000_0000_0002);

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL lock fixture error ({})", self.0)
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
        max_connections: 6,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s06-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

#[tokio::test]
async fn preserves_session_lock_compatibility_and_cleanup() -> Result<(), Box<dyn std::error::Error>>
{
    let config = local_config()?;
    let database = Database::connect(&config).await?;
    let second_database = Database::connect(&config).await?;
    let activity = AccountActivityPool::connect(&config).await?;
    let contender = AccountActivityPool::connect(&config).await?;

    cleanup_accounts(&database).await?;
    insert_account(&database, FIRST_ACCOUNT, "first").await?;
    insert_account(&database, SECOND_ACCOUNT, "second").await?;

    verify_account_eligibility(&database, &activity).await?;
    verify_exact_activity_key(&database, &activity).await?;
    verify_shared_and_exclusive_contention(&activity, &contender).await?;
    verify_cancelled_work_releases_the_session(&activity, &contender).await?;
    verify_connection_loss_invalidates_the_lease(&database, &activity, &contender).await?;
    verify_leader_lock_survives_commits(&database, &second_database).await?;

    wait_for_idle_pool(&activity).await?;
    assert!(activity.pool_stats().total <= 4);
    assert_eq!(activity.pool_stats().idle, activity.pool_stats().total);

    cleanup_accounts(&database).await?;
    activity.close().await;
    contender.close().await;
    second_database.close().await;
    database.close().await;
    Ok(())
}

async fn verify_account_eligibility(
    database: &Database,
    activity: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let missing = Uuid::from_u128(0x7100_0000_0000_4000_8000_0000_0000_0099);
    let result: Result<(), AccountActivityError> = activity
        .run(&[missing], |_lease| Box::pin(async { Ok(()) }))
        .await;
    assert_eq!(result, Err(AccountActivityError::AccountUnavailable));

    sqlx::query("UPDATE user_account SET is_banned = true WHERE user_id = $1")
        .bind(FIRST_ACCOUNT)
        .execute(database.acquire().await?.as_mut())
        .await?;
    let banned: Result<(), AccountActivityError> = activity
        .run(&[FIRST_ACCOUNT], |_lease| Box::pin(async { Ok(()) }))
        .await;
    assert_eq!(banned, Err(AccountActivityError::AccountUnavailable));
    let maintenance: Result<&str, AccountActivityError> = activity
        .run_existing(&[FIRST_ACCOUNT], |_lease| {
            Box::pin(async { Ok("protected") })
        })
        .await;
    assert_eq!(maintenance, Ok("protected"));

    sqlx::query("UPDATE user_account SET is_banned = false, deleted_at = now() WHERE user_id = $1")
        .bind(FIRST_ACCOUNT)
        .execute(database.acquire().await?.as_mut())
        .await?;
    let erased: Result<(), AccountActivityError> = activity
        .run_existing(&[FIRST_ACCOUNT], |_lease| Box::pin(async { Ok(()) }))
        .await;
    assert_eq!(erased, Err(AccountActivityError::AccountUnavailable));
    sqlx::query("UPDATE user_account SET deleted_at = NULL WHERE user_id = $1")
        .bind(FIRST_ACCOUNT)
        .execute(database.acquire().await?.as_mut())
        .await?;

    let duplicate: Result<&str, AccountActivityError> = activity
        .run(&[FIRST_ACCOUNT, FIRST_ACCOUNT], |_lease| {
            Box::pin(async { Ok("deduplicated") })
        })
        .await;
    assert_eq!(duplicate, Ok("deduplicated"));
    Ok(())
}

async fn verify_exact_activity_key(
    database: &Database,
    activity: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = database.acquire().await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 13092026))")
            .bind(FIRST_ACCOUNT.to_string().to_uppercase())
            .fetch_one(&mut *raw)
            .await?;
    assert!(locked);

    let blocked: Result<(), AccountActivityError> = activity
        .run(&[FIRST_ACCOUNT], |_lease| Box::pin(async { Ok(()) }))
        .await;
    assert_eq!(blocked, Err(AccountActivityError::AccountUnavailable));
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(&mut *raw)
        .await?;
    Ok(())
}

async fn verify_shared_and_exclusive_contention(
    activity: &AccountActivityPool,
    contender: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let acquired = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let holder_pool = activity.clone();
    let holder_acquired = Arc::clone(&acquired);
    let holder_release = Arc::clone(&release);
    let holder = tokio::spawn(async move {
        holder_pool
            .run::<(), AccountActivityError, _>(&[FIRST_ACCOUNT], move |lease| {
                Box::pin(async move {
                    lease.assert_held()?;
                    holder_acquired.notify_one();
                    holder_release.notified().await;
                    Ok(())
                })
            })
            .await
    });
    acquired.notified().await;

    let parallel_reader: Result<&str, AccountActivityError> = contender
        .run(&[FIRST_ACCOUNT], |_lease| Box::pin(async { Ok("shared") }))
        .await;
    assert_eq!(parallel_reader, Ok("shared"));
    let exclusive: TryExclusive<()> = contender
        .try_exclusive::<(), AccountActivityError, _>(FIRST_ACCOUNT, |_lease| {
            Box::pin(async { Ok(()) })
        })
        .await?;
    assert_eq!(exclusive, TryExclusive::NotAcquired);

    release.notify_one();
    holder.await??;
    let exclusive: TryExclusive<&str> = contender
        .try_exclusive::<&str, AccountActivityError, _>(FIRST_ACCOUNT, |_lease| {
            Box::pin(async { Ok("exclusive") })
        })
        .await?;
    assert_eq!(exclusive, TryExclusive::Acquired("exclusive"));
    Ok(())
}

async fn verify_cancelled_work_releases_the_session(
    activity: &AccountActivityPool,
    contender: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let acquired = Arc::new(Notify::new());
    let holder_pool = activity.clone();
    let holder_acquired = Arc::clone(&acquired);
    let holder = tokio::spawn(async move {
        holder_pool
            .run::<(), AccountActivityError, _>(&[SECOND_ACCOUNT], move |_lease| {
                Box::pin(async move {
                    holder_acquired.notify_one();
                    std::future::pending::<Result<(), AccountActivityError>>().await
                })
            })
            .await
    });
    acquired.notified().await;
    holder.abort();
    let _ = holder.await;

    timeout(Duration::from_secs(5), async {
        loop {
            let result = contender
                .try_exclusive::<(), AccountActivityError, _>(SECOND_ACCOUNT, |_lease| {
                    Box::pin(async { Ok(()) })
                })
                .await;
            if matches!(result, Ok(TryExclusive::Acquired(()))) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| FixtureError("cancelled_activity_cleanup"))?;
    Ok(())
}

async fn verify_connection_loss_invalidates_the_lease(
    database: &Database,
    activity: &AccountActivityPool,
    contender: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (process_sender, process_receiver) = oneshot::channel();
    let external_writes = Arc::new(AtomicU32::new(0));
    let guarded_pool = activity.clone();
    let guarded_writes = Arc::clone(&external_writes);
    let guarded = tokio::spawn(async move {
        guarded_pool
            .run::<(), AccountActivityError, _>(&[FIRST_ACCOUNT], move |lease| {
                Box::pin(async move {
                    let _ = process_sender.send(lease.backend_process_id());
                    loop {
                        sleep(Duration::from_millis(20)).await;
                        if lease.assert_held().is_err() {
                            lease.assert_held()?;
                            guarded_writes.fetch_add(1, Ordering::Relaxed);
                            return Ok(());
                        }
                    }
                })
            })
            .await
    });
    let backend_process_id = process_receiver.await?;
    let mut killer = database.acquire().await?;
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(backend_process_id)
        .fetch_one(&mut *killer)
        .await?;
    assert!(terminated);
    let guarded_result = timeout(Duration::from_secs(5), guarded)
        .await
        .map_err(|_| FixtureError("activity_connection_loss"))??;
    assert_eq!(
        guarded_result,
        Err(AccountActivityError::ActivityUnavailable)
    );
    assert_eq!(external_writes.load(Ordering::Relaxed), 0);

    let reacquired = contender
        .try_exclusive::<(), AccountActivityError, _>(FIRST_ACCOUNT, |_lease| {
            Box::pin(async { Ok(()) })
        })
        .await?;
    assert_eq!(reacquired, TryExclusive::Acquired(()));
    Ok(())
}

async fn verify_leader_lock_survives_commits(
    database: &Database,
    contender: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut leader = SessionLeaderLease::try_acquire_match_maintenance(database)
        .await?
        .ok_or(FixtureError("leader_initial_acquire"))?;
    assert!(
        SessionLeaderLease::try_acquire(contender, MATCH_MAINTENANCE_LOCK)
            .await?
            .is_none()
    );

    let mut transaction = leader.connection_mut().begin().await?;
    sqlx::query("SELECT 1").execute(&mut *transaction).await?;
    transaction.commit().await?;
    assert!(
        SessionLeaderLease::try_acquire(contender, MATCH_MAINTENANCE_LOCK)
            .await?
            .is_none()
    );

    leader.release().await?;
    let next = SessionLeaderLease::try_acquire(contender, MATCH_MAINTENANCE_LOCK)
        .await?
        .ok_or(FixtureError("leader_reacquire"))?;
    next.release().await?;

    let abandoned = SessionLeaderLease::try_acquire(database, MATCH_MAINTENANCE_LOCK)
        .await?
        .ok_or(FixtureError("leader_abandon_acquire"))?;
    drop(abandoned);
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(recovered) =
                SessionLeaderLease::try_acquire(contender, MATCH_MAINTENANCE_LOCK)
                    .await
                    .ok()
                    .flatten()
            {
                let _ = recovered.release().await;
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| FixtureError("abandoned_leader_cleanup"))?;
    Ok(())
}

async fn wait_for_idle_pool(
    activity: &AccountActivityPool,
) -> Result<(), Box<dyn std::error::Error>> {
    timeout(Duration::from_secs(5), async {
        loop {
            let stats = activity.pool_stats();
            if stats.total == stats.idle {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| FixtureError("activity_pool_release"))?;
    Ok(())
}

async fn insert_account(
    database: &Database,
    user_id: Uuid,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
            (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s06-lock-{suffix}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn cleanup_accounts(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(vec![FIRST_ACCOUNT, SECOND_ACCOUNT])
        .execute(&mut *connection)
        .await?;
    Ok(())
}
