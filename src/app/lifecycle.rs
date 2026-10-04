use std::{future::Future, time::Duration};
use tokio::{task::JoinSet, time};
use tokio_util::sync::CancellationToken;

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
            let mut first_error = None;
            while let Some(result) = self.tasks.join_next().await {
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) if error.is_panic() => Some(BootstrapError::TaskPanicked),
                    Err(_) => Some(BootstrapError::TaskCancelled),
                };
                if first_error.is_none() {
                    first_error = error;
                }
            }
            first_error.map_or(Ok(()), Err)
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

    /// Long-running workers must not silently disappear while the process stays alive.
    pub async fn wait_for_exit(&mut self) -> Result<(), BootstrapError> {
        match self.tasks.join_next().await {
            Some(Ok(Err(error))) => Err(error),
            Some(Err(error)) if error.is_panic() => Err(BootstrapError::TaskPanicked),
            Some(Err(_)) => Err(BootstrapError::TaskCancelled),
            Some(Ok(Ok(()))) => Err(BootstrapError::TaskFailed("task_exited")),
            None => Err(BootstrapError::TaskFailed("task_missing")),
        }
    }
}

impl Drop for TaskSupervisor {
    fn drop(&mut self) {
        self.cancellation.cancel();
        // JoinSet aborts remaining tasks when it is dropped.
    }
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
