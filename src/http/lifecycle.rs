use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, BodyDataStream, Bytes};
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use ipnet::IpNet;
use uuid::{Uuid, Variant, Version};

use crate::config::{Environment, LimitPolicy, TrustProxy};
use crate::operations::logging::{self, SafeLogValue};

use super::rate_limit::RateLimiter;

const STRIPE_WEBHOOK: &str = "/api/billing/stripe/webhook";
const SWEEGO_WEBHOOK: &str = "/api/auth/sweego/webhook";

#[derive(Clone)]
pub struct LifecycleState {
    environment: Environment,
    trust_proxy: TrustedProxies,
    limiter: RateLimiter,
    global_policy: LimitPolicy,
    observer: Arc<dyn HttpObserver>,
}

impl LifecycleState {
    pub fn new(
        environment: Environment,
        trust_proxy: &TrustProxy,
        limiter: RateLimiter,
        global_policy: LimitPolicy,
        observer: Arc<dyn HttpObserver>,
    ) -> Result<Self, TrustProxyError> {
        Ok(Self {
            environment,
            trust_proxy: TrustedProxies::new(trust_proxy)?,
            limiter,
            global_policy,
            observer,
        })
    }
}

#[derive(Clone, Debug)]
enum TrustedProxies {
    Disabled,
    All,
    Networks(Arc<[IpNet]>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrustProxyError;

impl TrustedProxies {
    fn new(config: &TrustProxy) -> Result<Self, TrustProxyError> {
        match config {
            TrustProxy::Disabled => Ok(Self::Disabled),
            TrustProxy::All => Ok(Self::All),
            TrustProxy::Networks(values) => values
                .iter()
                .map(|value| value.parse::<IpNet>().map_err(|_| TrustProxyError))
                .collect::<Result<Vec<_>, _>>()
                .map(|values| Self::Networks(values.into())),
        }
    }

    fn client_ip(&self, remote: IpAddr, headers: &HeaderMap) -> IpAddr {
        if matches!(self, Self::Disabled) || !self.contains(remote) {
            return remote;
        }
        let Some(forwarded) = forwarded_addresses(headers) else {
            return remote;
        };
        match self {
            Self::All => forwarded.first().copied().unwrap_or(remote),
            Self::Networks(_) => {
                let mut client = remote;
                for address in forwarded.iter().rev() {
                    if !self.contains(client) {
                        break;
                    }
                    client = *address;
                }
                client
            }
            Self::Disabled => remote,
        }
    }

    fn contains(&self, address: IpAddr) -> bool {
        match self {
            Self::Disabled => false,
            Self::All => true,
            Self::Networks(networks) => networks.iter().any(|network| network.contains(&address)),
        }
    }
}

fn forwarded_addresses(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let value = headers
        .get(HeaderName::from_static("x-forwarded-for"))?
        .to_str()
        .ok()?;
    value
        .split(',')
        .map(|part| part.trim().parse::<IpAddr>().ok())
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestId(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientIp(pub IpAddr);

#[derive(Clone, Debug)]
pub struct HttpObservation {
    pub method: String,
    pub route: String,
    pub status: StatusCode,
    pub request_id: String,
    pub duration_ms: f64,
}

pub trait HttpObserver: Send + Sync {
    fn record(&self, observation: HttpObservation);
}

pub struct CompositeHttpObserver {
    observers: Vec<Arc<dyn HttpObserver>>,
}

impl CompositeHttpObserver {
    pub fn new(observers: Vec<Arc<dyn HttpObserver>>) -> Self {
        Self { observers }
    }
}

impl HttpObserver for CompositeHttpObserver {
    fn record(&self, observation: HttpObservation) {
        for observer in &self.observers {
            observer.record(observation.clone());
        }
    }
}

#[derive(Default)]
pub struct SafeHttpObserver;

impl HttpObserver for SafeHttpObserver {
    fn record(&self, observation: HttpObservation) {
        if observation.status.as_u16() < 400 {
            return;
        }
        let duration = (observation.duration_ms * 10.0).round() / 10.0;
        let fields = [
            ("method", SafeLogValue::String(&observation.method)),
            ("route", SafeLogValue::String(&observation.route)),
            (
                "status",
                SafeLogValue::Integer(i64::from(observation.status.as_u16())),
            ),
            ("request_id", SafeLogValue::String(&observation.request_id)),
            ("duration_ms", SafeLogValue::Number(duration)),
        ];
        let line = logging::format_log_event("http_request_failed", &fields);
        if let Ok(line) = line {
            if observation.status.is_server_error() {
                tracing::error!(target: "histae", message = %line);
            } else {
                tracing::warn!(target: "histae", message = %line);
            }
        }
    }
}

pub async fn middleware(
    State(state): State<LifecycleState>,
    mut request: Request,
    next: Next,
) -> Response {
    let started_at = Instant::now();
    let request_id = request_id(request.headers());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let remote = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |info| info.0.ip());
    let client_ip = state.trust_proxy.client_ip(remote, request.headers());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    request.extensions_mut().insert(ClientIp(client_ip));

    let mut response = if path == STRIPE_WEBHOOK || path == SWEEGO_WEBHOOK {
        next.run(request).await
    } else {
        match state
            .limiter
            .enforce(
                "global",
                &client_ip.to_string(),
                &state.global_policy,
                "rate_limit_exceeded",
            )
            .await
        {
            Ok(()) => next.run(request).await,
            Err(error) => error.into_response(),
        }
    };
    apply_security_headers(response.headers_mut(), state.environment);
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    let pending = PendingObservation {
        observer: Arc::clone(&state.observer),
        started_at,
        method,
        route,
        status: response.status(),
        request_id,
    };
    if is_streaming_response(&response) {
        let body = std::mem::replace(response.body_mut(), Body::empty());
        *response.body_mut() = Body::from_stream(ObservedBodyStream {
            inner: Box::pin(body.into_data_stream()),
            pending: Some(pending),
        });
    } else {
        pending.record();
    }
    response
}

fn is_streaming_response(response: &Response) -> bool {
    let event_stream = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/event-stream"))
        });
    let attachment = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("attachment"))
        });
    event_stream || attachment
}

struct PendingObservation {
    observer: Arc<dyn HttpObserver>,
    started_at: Instant,
    method: String,
    route: String,
    status: StatusCode,
    request_id: String,
}

impl PendingObservation {
    fn record(self) {
        self.observer.record(HttpObservation {
            method: self.method,
            route: self.route,
            status: self.status,
            request_id: self.request_id,
            duration_ms: self.started_at.elapsed().as_secs_f64() * 1_000.0,
        });
    }
}

struct ObservedBodyStream {
    inner: Pin<Box<BodyDataStream>>,
    pending: Option<PendingObservation>,
}

impl ObservedBodyStream {
    fn record(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.record();
        }
    }
}

impl Stream for ObservedBodyStream {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(context);
        if matches!(polled, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.record();
        }
        polled
    }
}

impl Drop for ObservedBodyStream {
    fn drop(&mut self) {
        self.record();
    }
}

pub fn apply_security_headers(headers: &mut HeaderMap, environment: Environment) {
    const VALUES: &[(&str, &str)] = &[
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "base-uri 'none'; frame-ancestors 'none'; object-src 'none'",
        ),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()",
        ),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-dns-prefetch-control", "off"),
        ("x-frame-options", "DENY"),
        ("x-permitted-cross-domain-policies", "none"),
    ];
    for (name, value) in VALUES {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    if environment == Environment::Production {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(HeaderName::from_static("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| {
            value.get_version() == Some(Version::Random) && value.get_variant() == Variant::RFC4122
        })
        .map(|value| value.hyphenated().to_string())
        .unwrap_or_else(|| Uuid::new_v4().hyphenated().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingObserver(AtomicUsize);

    impl HttpObserver for CountingObserver {
        fn record(&self, _observation: HttpObservation) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn resolves_proxy_chains_from_the_socket_towards_the_client() {
        let networks = TrustedProxies::new(&TrustProxy::Networks(vec![
            "10.0.0.0/8".to_owned(),
            "192.168.0.0/16".to_owned(),
        ]))
        .expect("valid test networks");
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.4, 192.168.1.20"),
        );
        assert_eq!(
            networks.client_ip("10.0.0.2".parse().expect("valid IP"), &headers),
            "203.0.113.4".parse::<IpAddr>().expect("valid IP")
        );
        assert_eq!(
            networks.client_ip("198.51.100.2".parse().expect("valid IP"), &headers),
            "198.51.100.2".parse::<IpAddr>().expect("valid IP")
        );
    }

    #[test]
    fn accepts_only_rfc4122_uuid_v4_request_ids() {
        let mut headers = HeaderMap::new();
        let valid = Uuid::new_v4().to_string().to_ascii_uppercase();
        headers.insert(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_str(&valid).expect("generated request ID header"),
        );
        assert_eq!(request_id(&headers), valid.to_ascii_lowercase());
        let mut wrong_version = Uuid::new_v4().to_string();
        wrong_version.replace_range(14..15, "1");
        headers.insert(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_str(&wrong_version).expect("generated invalid request ID header"),
        );
        assert_ne!(request_id(&headers), wrong_version);
    }

    #[test]
    fn records_a_stream_observation_when_the_client_drops_the_body() {
        let observer = Arc::new(CountingObserver::default());
        let stream = ObservedBodyStream {
            inner: Box::pin(Body::from("event: connected\n\n").into_data_stream()),
            pending: Some(PendingObservation {
                observer: observer.clone(),
                started_at: Instant::now(),
                method: "GET".to_owned(),
                route: "/api/users/me/events".to_owned(),
                status: StatusCode::OK,
                request_id: Uuid::new_v4().to_string(),
            }),
        };
        assert_eq!(observer.0.load(Ordering::Relaxed), 0);
        drop(stream);
        assert_eq!(observer.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn defers_observation_for_download_attachments_as_well_as_event_streams() {
        let event_stream = Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .body(Body::empty())
            .expect("event stream response");
        let attachment = Response::builder()
            .header(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"histae-data-export.json\"",
            )
            .body(Body::empty())
            .expect("attachment response");
        let ordinary = Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .expect("ordinary response");

        assert!(is_streaming_response(&event_stream));
        assert!(is_streaming_response(&attachment));
        assert!(!is_streaming_response(&ordinary));
    }
}
