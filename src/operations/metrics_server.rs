use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::MetricsConfig;
use crate::operations::logging;

use super::prometheus::MetricsRenderer;

#[derive(Clone)]
struct ServerState {
    token: Arc<[u8]>,
    renderer: Arc<dyn MetricsRenderer>,
}

pub struct MetricsServer {
    cancellation: CancellationToken,
    task: JoinHandle<Result<(), &'static str>>,
    local_port: u16,
}

impl MetricsServer {
    pub async fn start(
        config: &MetricsConfig,
        renderer: Arc<dyn MetricsRenderer>,
    ) -> Result<Option<Self>, &'static str> {
        if !config.enabled {
            return Ok(None);
        }
        let listener = TcpListener::bind((config.host.as_str(), config.port))
            .await
            .map_err(|_| "metrics_bind_failed")?;
        let local_port = listener
            .local_addr()
            .map_err(|_| "metrics_bind_failed")?
            .port();
        let state = ServerState {
            token: Arc::from(config.token.expose_secret().as_bytes()),
            renderer,
        };
        let router = Router::new().fallback(handler).with_state(state);
        let cancellation = CancellationToken::new();
        let shutdown = cancellation.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .map_err(|_| "metrics_server_failed")
        });
        let _ = logging::info(
            "metrics_server_started",
            &[(
                "port",
                logging::SafeLogValue::Integer(i64::from(local_port)),
            )],
        );
        Ok(Some(Self {
            cancellation,
            task,
            local_port,
        }))
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub async fn shutdown(self) -> Result<(), &'static str> {
        self.cancellation.cancel();
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("metrics_server_task_failed"),
            Err(_) => Err("metrics_server_shutdown_timed_out"),
        }
    }
}

async fn handler(State(state): State<ServerState>, request: Request) -> Response {
    let exact_path = request
        .uri()
        .path_and_query()
        .is_some_and(|value| value.as_str() == "/metrics");
    if !exact_path {
        return plain(StatusCode::NOT_FOUND, "Not Found\n");
    }
    if request.method() != Method::GET {
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET"));
        return response;
    }
    if !authorized(
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        &state.token,
    ) {
        let mut response = plain(StatusCode::UNAUTHORIZED, "Unauthorized\n");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    match state.renderer.render().await {
        Ok(body) => {
            let mut response = plain(StatusCode::OK, body);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(_) => plain(StatusCode::SERVICE_UNAVAILABLE, "Metrics unavailable\n"),
    }
}

fn authorized(header: Option<&str>, expected: &[u8]) -> bool {
    let Some(candidate) = header.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    candidate.len() == expected.len() && candidate.as_bytes().ct_eq(expected).into()
}

fn plain(status: StatusCode, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::future::Future;
    use std::pin::Pin;
    use tower::ServiceExt;

    struct Renderer(Result<&'static str, &'static str>);
    impl MetricsRenderer for Renderer {
        fn render(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<String, &'static str>> + Send + '_>> {
            Box::pin(async move { self.0.map(str::to_owned) })
        }
    }
    fn app(renderer: Result<&'static str, &'static str>) -> Router {
        Router::new().fallback(handler).with_state(ServerState {
            token: Arc::from(b"01234567890123456789012345678901".as_slice()),
            renderer: Arc::new(Renderer(renderer)),
        })
    }
    async fn call(
        method: Method,
        uri: &str,
        auth: Option<&str>,
        renderer: Result<&'static str, &'static str>,
    ) -> (StatusCode, String, axum::http::HeaderMap) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(value) = auth {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app(renderer)
            .oneshot(builder.body(Body::empty()).expect("test request"))
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            String::from_utf8(body.to_vec()).expect("utf8"),
            headers,
        )
    }
    #[tokio::test]
    async fn enforces_exact_path_method_and_bearer_token() {
        assert_eq!(
            call(Method::GET, "/metrics?x=1", None, Ok("ok")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(Method::POST, "/metrics", None, Ok("ok")).await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            call(Method::GET, "/metrics", None, Ok("ok")).await.0,
            StatusCode::UNAUTHORIZED
        );
        let result = call(
            Method::GET,
            "/metrics",
            Some("Bearer 01234567890123456789012345678901"),
            Ok("metric 1\n"),
        )
        .await;
        assert_eq!(
            (result.0, result.1.as_str()),
            (StatusCode::OK, "metric 1\n")
        );
        assert_eq!(result.2[header::CACHE_CONTROL], "no-store");
    }
    #[tokio::test]
    async fn normalizes_renderer_failures() {
        let result = call(
            Method::GET,
            "/metrics",
            Some("Bearer 01234567890123456789012345678901"),
            Err("private"),
        )
        .await;
        assert_eq!(
            (result.0, result.1.as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "Metrics unavailable\n")
        );
    }
}
