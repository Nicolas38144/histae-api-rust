use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time;
use tokio_util::sync::CancellationToken;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapError {
    TaskFailed(&'static str),
    TaskPanicked,
    TaskCancelled,
    StartupTimedOut,
    ShutdownTimedOut,
}

impl BootstrapError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::TaskFailed(code) => code,
            Self::TaskPanicked => "task_panicked",
            Self::TaskCancelled => "task_cancelled",
            Self::StartupTimedOut => "startup_timed_out",
            Self::ShutdownTimedOut => "shutdown_timed_out",
        }
    }
}

pub async fn bounded_start<F, T>(budget: Duration, startup: F) -> Result<T, BootstrapError>
where
    F: Future<Output = Result<T, BootstrapError>>,
{
    time::timeout(budget, startup)
        .await
        .map_err(|_| BootstrapError::StartupTimedOut)?
}

pub struct TaskSupervisor {
    cancellation: CancellationToken,
    tasks: JoinSet<Result<(), BootstrapError>>,
    drain_timeout: Duration,
}

impl TaskSupervisor {
    pub fn new(drain_timeout: Duration) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            tasks: JoinSet::new(),
            drain_timeout,
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = Result<(), BootstrapError>> + Send + 'static,
    {
        self.tasks.spawn(task);
    }

    pub async fn shutdown(mut self) -> Result<(), BootstrapError> {
        self.cancellation.cancel();
        let drain = async {
            while let Some(result) = self.tasks.join_next().await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => return Err(error),
                    Err(error) if error.is_panic() => return Err(BootstrapError::TaskPanicked),
                    Err(_) => return Err(BootstrapError::TaskCancelled),
                }
            }
            Ok(())
        };
        match time::timeout(self.drain_timeout, drain).await {
            Ok(result) => result,
            Err(_) => {
                self.tasks.abort_all();
                while self.tasks.join_next().await.is_some() {}
                Err(BootstrapError::ShutdownTimedOut)
            }
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
    let _ = logging::error(component.failure_event(), Some("component_not_implemented"));
    ExitCode::FAILURE
}

fn requires_worker_mode(component: Component) -> bool {
    matches!(component, Component::Outbox | Component::Maintenance)
}

pub async fn wait_for_shutdown_signal() -> Result<(), BootstrapError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| BootstrapError::TaskFailed("signal_setup_failed"))?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| BootstrapError::TaskFailed("signal_wait_failed")),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(windows)]
    {
        let mut ctrl_break = tokio::signal::windows::ctrl_break()
            .map_err(|_| BootstrapError::TaskFailed("signal_setup_failed"))?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| BootstrapError::TaskFailed("signal_wait_failed")),
            _ = ctrl_break.recv() => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
