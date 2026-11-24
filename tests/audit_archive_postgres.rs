#![cfg(all(feature = "postgres-integration", unix))]

use std::{path::PathBuf, process::Stdio, time::Duration};

use histae_api_rust::{config::AppConfig, infra::postgres::Database};
use tokio::process::Command;
use uuid::Uuid;

struct ArchiveDir(PathBuf);

impl Drop for ArchiveDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn app_can_append_but_only_the_archive_service_can_export_and_purge()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("HISTAE_AUDIT_ARCHIVE_TEST_ISOLATED").as_deref() != Ok("true") {
        return Ok(());
    }
    let config = AppConfig::from_env()?.postgres;
    assert_eq!(config.user, "histae_app");
    let database = Database::connect(&config).await?;
    let mut connection = database.acquire().await?;
    let protected: bool = sqlx::query_scalar(
        "SELECT
      NOT has_table_privilege('public.data_access_log','UPDATE,DELETE,TRUNCATE')
      AND NOT has_table_privilege('public.admin_auth_event','UPDATE,DELETE,TRUNCATE')
      AND NOT has_table_privilege('public.audit_archive_pending','SELECT,INSERT,UPDATE,DELETE')
      AND NOT has_function_privilege('public.purge_archived_audit_events(integer)','EXECUTE')",
    )
    .fetch_one(&mut *connection)
    .await?;
    assert!(protected);

    // The durable copy must survive both account deletion and the usual purge.
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted) VALUES($1,$2,''::bytea)")
        .bind(account).bind(format!("audit-fixture-{account}")).execute(&mut *connection).await?;
    let auth_event: (Uuid, chrono::DateTime<chrono::Utc>) = sqlx::query_as("INSERT INTO admin_auth_event(user_id,event_type) VALUES($1,'login_succeeded') RETURNING id,created_at")
        .bind(account).fetch_one(&mut *connection).await?;
    let old_access: Uuid = sqlx::query_scalar("INSERT INTO data_access_log(accessed_user_id,accessor_id,action,accessed_at) VALUES($1,$1,'view_profile',clock_timestamp()-interval '1 year 1 day') RETURNING id")
        .bind(account).fetch_one(&mut *connection).await?;
    sqlx::query("DELETE FROM user_account WHERE user_id=$1")
        .bind(account)
        .execute(&mut *connection)
        .await?;
    let still_present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM data_access_log WHERE id=$1 AND accessor_id=$2) AND EXISTS(SELECT 1 FROM admin_auth_event WHERE id=$3 AND user_id=$2)")
        .bind(old_access).bind(account).bind(auth_event.0).fetch_one(&mut *connection).await?;
    assert!(still_present);

    use std::os::unix::fs::DirBuilderExt as _;
    let directory =
        ArchiveDir(std::env::temp_dir().join(format!("histae-audit-fixture-{}", Uuid::new_v4())));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory.0)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_audit-archive"))
        .env_clear()
        .env("ENV", "test")
        .env("POSTGRES_HOST", &config.host)
        .env("POSTGRES_PORT", config.port.to_string())
        .env("POSTGRES_DB", &config.database)
        .env("POSTGRES_USER", "histae_audit_archive")
        .env(
            "POSTGRES_PASSWORD",
            std::env::var("AUDIT_ARCHIVE_TEST_PASSWORD")?,
        )
        .env("AUDIT_ARCHIVE_DIR", &directory.0)
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let archived_file = directory
        .0
        .join(auth_event.1.format("%Y-%m-%d").to_string())
        .join(format!("admin_auth_event-{}.json", auth_event.0));
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            assert!(child.try_wait()?.is_none(), "archive service exited");
            let purged: bool =
                sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM data_access_log WHERE id=$1)")
                    .bind(old_access)
                    .fetch_one(&mut *connection)
                    .await?;
            if purged && archived_file.is_file() {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await;
    child.kill().await?;
    result??;
    let archived: serde_json::Value = serde_json::from_slice(&std::fs::read(archived_file)?)?;
    assert_eq!(archived["user_id"], account.to_string());
    assert_eq!(archived["event_type"], "login_succeeded");
    Ok(())
}
