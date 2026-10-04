pub mod lifecycle;
pub use lifecycle::{BootstrapError, TaskSupervisor, bounded_start, wait_for_shutdown_signal};

#[cfg(feature = "webauthn-probe")]
pub mod api;
pub mod maintenance;
pub mod outbox;

use std::process::ExitCode;

use crate::config::{AppConfig, MaintenanceMode};
use crate::operations::logging;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Component {
    Api,
    Outbox,
    Maintenance,
    AdminBootstrap,
}

impl Component {
    fn failure_event(self) -> &'static str {
        match self {
            Self::Api => "api_bootstrap_failed",
            Self::Outbox => "outbox_worker_failed",
            Self::Maintenance => "maintenance_failed",
            Self::AdminBootstrap => "admin_webauthn_bootstrap_failed",
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Outbox => "outbox",
            Self::Maintenance => "maintenance",
            Self::AdminBootstrap => "admin_bootstrap",
        }
    }
}

pub async fn binary_main(component: Component) -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let config = match AppConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            let _ = logging::error(component.failure_event(), Some(error.safe_code()));
            return ExitCode::FAILURE;
        }
    };
    if requires_worker_mode(component) && config.maintenance_mode != MaintenanceMode::Worker {
        let _ = logging::error(component.failure_event(), Some("invalid_maintenance_mode"));
        return ExitCode::FAILURE;
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--check-config"))
        && std::env::args_os().nth(2).is_none()
    {
        let _ = logging::info(
            "configuration_validated",
            &[
                (
                    "operation",
                    logging::SafeLogValue::String(component.operation()),
                ),
                (
                    "environment",
                    logging::SafeLogValue::String(config.environment.as_str()),
                ),
            ],
        );
        return ExitCode::SUCCESS;
    }
    let result = match component {
        Component::Outbox => crate::app::outbox::run(config).await,
        Component::Maintenance => crate::app::maintenance::run(config).await,
        Component::Api => {
            #[cfg(feature = "webauthn-probe")]
            {
                crate::app::api::run(config).await
            }
            #[cfg(not(feature = "webauthn-probe"))]
            {
                Err("webauthn_feature_required")
            }
        }
        Component::AdminBootstrap => Err("component_not_implemented"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let _ = logging::error(component.failure_event(), Some(code));
            ExitCode::FAILURE
        }
    }
}

fn requires_worker_mode(component: Component) -> bool {
    matches!(component, Component::Outbox | Component::Maintenance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn task_failure_does_not_interrupt_other_tasks_cleanup() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let mut supervisor = TaskSupervisor::new(Duration::from_secs(1));
        let cancellation = supervisor.cancellation_token();
        let completed = Arc::new(AtomicBool::new(false));
        let cleanup = completed.clone();
        supervisor.spawn(async { Err(BootstrapError::TaskFailed("dependency_unavailable")) });
        supervisor.spawn(async move {
            cancellation.cancelled().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            cleanup.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(
            supervisor.shutdown().await,
            Err(BootstrapError::TaskFailed("dependency_unavailable"))
        );
        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn unexpected_worker_exit_is_observable() {
        let mut supervisor = TaskSupervisor::new(Duration::from_secs(1));
        supervisor.spawn(async { Ok(()) });
        assert_eq!(
            supervisor.wait_for_exit().await,
            Err(BootstrapError::TaskFailed("task_exited"))
        );
        assert_eq!(supervisor.shutdown().await, Ok(()));
    }

    #[tokio::test]
    async fn dropping_the_supervisor_signals_cancellation() {
        let supervisor = TaskSupervisor::new(Duration::from_secs(1));
        let cancellation = supervisor.cancellation_token();
        drop(supervisor);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn preserves_the_existing_failure_event_names() {
        assert_eq!(Component::Api.failure_event(), "api_bootstrap_failed");
        assert_eq!(Component::Outbox.failure_event(), "outbox_worker_failed");
        assert_eq!(Component::Maintenance.failure_event(), "maintenance_failed");
        assert_eq!(
            Component::AdminBootstrap.failure_event(),
            "admin_webauthn_bootstrap_failed"
        );
    }

    #[tokio::test]
    async fn drains_cooperative_tasks() {
        let mut supervisor = TaskSupervisor::new(Duration::from_millis(100));
        let cancellation = supervisor.cancellation_token();
        supervisor.spawn(async move {
            cancellation.cancelled().await;
            Ok(())
        });
        assert_eq!(supervisor.shutdown().await, Ok(()));
    }

    #[tokio::test]
    async fn bounds_startup_before_a_component_can_report_ready() {
        let result = bounded_start(Duration::from_millis(10), async {
            std::future::pending::<()>().await;
            Ok(())
        })
        .await;
        assert_eq!(result, Err(BootstrapError::StartupTimedOut));
    }

    #[tokio::test]
    async fn aborts_tasks_after_the_bounded_drain() {
        let mut supervisor = TaskSupervisor::new(Duration::from_millis(10));
        supervisor.spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        });
        assert_eq!(
            supervisor.shutdown().await,
            Err(BootstrapError::ShutdownTimedOut)
        );
    }

    #[tokio::test]
    async fn surfaces_task_failures_as_codes_only() {
        let mut supervisor = TaskSupervisor::new(Duration::from_millis(100));
        supervisor.spawn(async { Err(BootstrapError::TaskFailed("dependency_unavailable")) });
        assert_eq!(
            supervisor.shutdown().await,
            Err(BootstrapError::TaskFailed("dependency_unavailable"))
        );
    }
}
