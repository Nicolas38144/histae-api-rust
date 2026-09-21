#[cfg(feature = "webauthn-probe")]
use std::io::Write as _;
#[cfg(feature = "webauthn-probe")]
use std::process::ExitCode;
#[cfg(feature = "webauthn-probe")]
use std::sync::Arc;

#[cfg(feature = "webauthn-probe")]
use histae_api_rust::config::AppConfig;
#[cfg(feature = "webauthn-probe")]
use histae_api_rust::identity::admin::pg::AdminAuthRepository;
#[cfg(feature = "webauthn-probe")]
use histae_api_rust::identity::admin::service::AdminAuthService;
#[cfg(feature = "webauthn-probe")]
use histae_api_rust::infra::postgres::Database;
#[cfg(feature = "webauthn-probe")]
use histae_api_rust::operations::logging;
#[cfg(feature = "webauthn-probe")]
use uuid::{Uuid, Variant};

#[cfg(feature = "webauthn-probe")]
#[tokio::main]
async fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let _ = logging::error("admin_webauthn_bootstrap_failed", Some(code));
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "webauthn-probe")]
async fn run() -> Result<(), &'static str> {
    let raw_user_id = std::env::args()
        .nth(1)
        .filter(|_| std::env::args().nth(2).is_none())
        .ok_or("invalid_admin_bootstrap_argument")?;
    let user_id =
        Uuid::parse_str(raw_user_id.trim()).map_err(|_| "invalid_admin_bootstrap_argument")?;
    if !(1..=8).contains(&user_id.get_version_num()) || user_id.get_variant() != Variant::RFC4122 {
        return Err("invalid_admin_bootstrap_argument");
    }
    let config = AppConfig::from_env().map_err(|_| "configuration_invalid")?;
    let database = Database::connect(&config.postgres)
        .await
        .map_err(|_| "postgres_unavailable")?;
    let repository = Arc::new(AdminAuthRepository::new(database.clone()));
    let service = AdminAuthService::new(repository, config.admin_auth.clone())
        .map_err(|_| "admin_webauthn_configuration_invalid")?;
    let issued = service
        .issue_bootstrap(user_id)
        .await
        .map_err(|_| "admin_bootstrap_not_created")?;
    let result = writeln!(
        std::io::stdout().lock(),
        "Administrator enrollment token (shown once, expires {}):\n{}",
        issued
            .expires_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        issued.token
    )
    .map_err(|_| "admin_bootstrap_output_failed");
    database.close().await;
    result
}

#[cfg(not(feature = "webauthn-probe"))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
