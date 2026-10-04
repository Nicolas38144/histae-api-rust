mod resources;
mod router;

use crate::app::wait_for_shutdown_signal;
use crate::config::AppConfig;
use crate::operations::{
    logging::{self, SafeLogValue},
    metrics_server::MetricsServer,
};
use resources::ApiResources;
use std::{future::IntoFuture, time::Duration};
use tokio::{net::TcpListener, time};

const API_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(config: AppConfig) -> Result<(), &'static str> {
    let resources = ApiResources::connect(&config).await?;
    let router = resources.router(&config)?;
    let listener = TcpListener::bind(("0.0.0.0", config.port))
        .await
        .map_err(|_| "api_bind_failed")?;
    let port = listener.local_addr().map_err(|_| "api_bind_failed")?.port();

    let metrics = MetricsServer::start(
        &config.metrics,
        resources.metrics_renderer(config.sms.otp_ttl),
    )
    .await
    .map_err(|_| "metrics_server_start_failed")?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    logging::info(
        "api_started",
        &[("port", SafeLogValue::Integer(i64::from(port)))],
    )
    .map_err(|_| "api_log_failed")?;

    let result = {
        let shutdown = cancellation.clone();
        let server = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            signal = wait_for_shutdown_signal() => match signal {
                Ok(()) => {
                    resources.realtime.shutdown();
                    cancellation.cancel();
                    match time::timeout(API_DRAIN_TIMEOUT, &mut server).await {
                        Ok(result) => result.map_err(|_| "api_server_failed"),
                        Err(_) => Err("api_shutdown_timed_out"),
                    }
                }
                Err(error) => Err(error.safe_code()),
            },
            result = &mut server => result.map_err(|_| "api_server_failed"),
        }
    };
    resources.realtime.shutdown();
    cancellation.cancel();
    let _ = logging::info("api_http_stopped", &[]);
    let metrics_result = match metrics {
        Some(server) => server.shutdown().await,
        None => Ok(()),
    };
    let _ = logging::info("api_metrics_stopped", &[]);
    resources.close().await;
    let _ = logging::info("api_resources_closed", &[]);
    result?;
    metrics_result
}
