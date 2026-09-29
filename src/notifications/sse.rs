use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::Extension;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use chrono::{SecondsFormat, Utc};
use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};
use uuid::Uuid;

use crate::http::router::HttpState;
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState};
use crate::identity::mobile::pg::MobileSessionRepository;
use crate::infra::postgres::DatabaseError;
use crate::infra::redis::{RedisError, RedisService, RedisSubscription};

pub const MOBILE_EVENTS_CHANNEL: &str = "histae:mobile-events:v1";
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const BROADCAST_CAPACITY: usize = 128;
const REDIS_RELAY_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MobileEventType {
    #[serde(rename = "match.created")]
    MatchCreated,
    #[serde(rename = "match.updated")]
    MatchUpdated,
    #[serde(rename = "matches.invalidated")]
    MatchesInvalidated,
    #[serde(rename = "message.created")]
    MessageCreated,
    #[serde(rename = "message.read")]
    MessageRead,
    #[serde(rename = "subscription.updated")]
    SubscriptionUpdated,
}

impl MobileEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MatchCreated => "match.created",
            Self::MatchUpdated => "match.updated",
            Self::MatchesInvalidated => "matches.invalidated",
            Self::MessageCreated => "message.created",
            Self::MessageRead => "message.read",
            Self::SubscriptionUpdated => "subscription.updated",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MobileEvent {
    pub id: String,
    pub user_id: String,
    #[serde(rename = "type")]
    pub kind: MobileEventType,
    pub occurred_at: String,
    pub data: Map<String, Value>,
}

#[derive(Clone)]
pub struct RealtimeService {
    inner: Arc<RealtimeInner>,
}

struct RealtimeInner {
    redis: RedisService,
    events: broadcast::Sender<MobileEvent>,
    _subscription: Option<RedisSubscription>,
    relay: Option<JoinHandle<()>>,
}

impl Drop for RealtimeInner {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.abort();
        }
    }
}

impl RealtimeService {
    pub async fn connect(redis: RedisService) -> Result<Self, RedisError> {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (subscription, relay) = if redis.enabled() {
            let (sender, mut receiver) = mpsc::channel(REDIS_RELAY_CAPACITY);
            let subscription = redis
                .subscribe(MOBILE_EVENTS_CHANNEL.to_owned(), sender)
                .await?;
            let local = events.clone();
            let relay = tokio::spawn(async move {
                while let Some(message) = receiver.recv().await {
                    match parse_event(&message) {
                        Some(event) => {
                            let _ = local.send(event);
                        }
                        None => tracing::warn!(event_code = "realtime_event_invalid"),
                    }
                }
            });
            (Some(subscription), Some(relay))
        } else {
            (None, None)
        };
        Ok(Self {
            inner: Arc::new(RealtimeInner {
                redis,
                events,
                _subscription: subscription,
                relay,
            }),
        })
    }

    pub async fn emit(
        &self,
        user_ids: &[Uuid],
        kind: MobileEventType,
        data: Map<String, Value>,
    ) -> Result<(), RedisError> {
        let occurred_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut recipients = user_ids.to_vec();
        recipients.sort_unstable();
        recipients.dedup();
        for user_id in recipients {
            let event = MobileEvent {
                id: Uuid::new_v4().hyphenated().to_string(),
                user_id: user_id.hyphenated().to_string(),
                kind,
                occurred_at: occurred_at.clone(),
                data: data.clone(),
            };
            if self.inner.redis.enabled() {
                let encoded =
                    serde_json::to_string(&event).map_err(|_| RedisError::CommandFailed)?;
                self.inner
                    .redis
                    .publish(MOBILE_EVENTS_CHANNEL, &encoded)
                    .await?;
            } else {
                let _ = self.inner.events.send(event);
            }
        }
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MobileEvent> {
        self.inner.events.subscribe()
    }
}

fn parse_event(value: &str) -> Option<MobileEvent> {
    let event = serde_json::from_str::<MobileEvent>(value).ok()?;
    if event.id.is_empty() || event.user_id.is_empty() || event.occurred_at.is_empty() {
        return None;
    }
    Some(event)
}

pub type SessionActivityFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, DatabaseError>> + Send + 'a>>;

pub trait SessionActivity: Send + Sync {
    fn is_active(&self, user_id: Uuid, session_id: Uuid) -> SessionActivityFuture<'_>;
}

impl SessionActivity for MobileSessionRepository {
    fn is_active(&self, user_id: Uuid, session_id: Uuid) -> SessionActivityFuture<'_> {
        Box::pin(MobileSessionRepository::is_active(
            self, user_id, session_id,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SsePolicy {
    pub heartbeat_interval: Duration,
}

impl Default for SsePolicy {
    fn default() -> Self {
        Self {
            heartbeat_interval: HEARTBEAT_INTERVAL,
        }
    }
}

#[derive(Clone)]
pub struct SseHttpState {
    realtime: RealtimeService,
    sessions: Arc<dyn SessionActivity>,
    policy: SsePolicy,
}

impl SseHttpState {
    pub fn new(realtime: RealtimeService, sessions: Arc<dyn SessionActivity>) -> Self {
        Self {
            realtime,
            sessions,
            policy: SsePolicy::default(),
        }
    }

    pub fn with_policy(
        realtime: RealtimeService,
        sessions: Arc<dyn SessionActivity>,
        policy: SsePolicy,
    ) -> Self {
        Self {
            realtime,
            sessions,
            policy,
        }
    }
}

pub fn routes(state: SseHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/events", get(events))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn events(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<SseHttpState>,
) -> Sse<BoxStream<'static, Result<Event, Infallible>>> {
    let stream = event_stream(
        state.realtime.subscribe(),
        state.sessions,
        identity.account.user_id,
        identity.session_id,
        identity.access_expires_at_seconds,
        state.policy,
    );
    Sse::new(stream)
}

struct StreamState {
    receiver: broadcast::Receiver<MobileEvent>,
    sessions: Arc<dyn SessionActivity>,
    user_id: Uuid,
    session_id: Uuid,
    expiry: Instant,
    heartbeat: time::Interval,
    session_check: time::Interval,
    connected: bool,
}

fn event_stream(
    receiver: broadcast::Receiver<MobileEvent>,
    sessions: Arc<dyn SessionActivity>,
    user_id: Uuid,
    session_id: Uuid,
    access_expires_at_seconds: u64,
    policy: SsePolicy,
) -> BoxStream<'static, Result<Event, Infallible>> {
    let until_expiry = access_expires_at_seconds.saturating_sub(unix_seconds());
    let mut heartbeat = time::interval(policy.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    heartbeat.reset();
    let mut session_check = time::interval(policy.heartbeat_interval);
    session_check.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let state = StreamState {
        receiver,
        sessions,
        user_id,
        session_id,
        expiry: Instant::now() + Duration::from_secs(until_expiry),
        heartbeat,
        session_check,
        connected: false,
    };
    stream::unfold(state, |mut state| async move {
        if !state.connected {
            state.connected = true;
            return Some((Ok(server_event("connected")), state));
        }
        loop {
            tokio::select! {
                _ = time::sleep_until(state.expiry) => return None,
                _ = state.session_check.tick() => {
                    if !state.sessions.is_active(state.user_id, state.session_id).await.unwrap_or(false) {
                        return None;
                    }
                }
                _ = state.heartbeat.tick() => {
                    return Some((Ok(server_event("heartbeat")), state));
                }
                received = state.receiver.recv() => {
                    match received {
                        Ok(event) if event.user_id == state.user_id.hyphenated().to_string() => {
                            return Some((Ok(mobile_event(event)), state));
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => return None,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        }
    })
    .boxed()
}

fn server_event(kind: &'static str) -> Event {
    Event::default().event(kind).data(
        serde_json::to_string(&json!({
            "server_time": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
        }))
        .unwrap_or_else(|_| "{}".to_owned()),
    )
}

fn mobile_event(event: MobileEvent) -> Event {
    let mut data = Map::from_iter([("occurred_at".to_owned(), Value::String(event.occurred_at))]);
    data.extend(event.data);
    Event::default()
        .id(event.id)
        .event(event.kind.as_str())
        .data(serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_owned()))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Sessions(AtomicBool);

    impl SessionActivity for Sessions {
        fn is_active(&self, _user_id: Uuid, _session_id: Uuid) -> SessionActivityFuture<'_> {
            let active = self.0.load(Ordering::Relaxed);
            Box::pin(async move { Ok(active) })
        }
    }

    #[tokio::test]
    async fn fallback_delivers_only_to_each_distinct_recipient() {
        let realtime = RealtimeService::connect(RedisService::disabled())
            .await
            .unwrap_or_else(|_| unreachable!());
        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut receiver = realtime.subscribe();
        realtime
            .emit(
                &[other, user, user],
                MobileEventType::MessageCreated,
                Map::from_iter([("match_id".to_owned(), json!(Uuid::new_v4()))]),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        let first = receiver.recv().await.unwrap_or_else(|_| unreachable!());
        let second = receiver.recv().await.unwrap_or_else(|_| unreachable!());
        assert_ne!(first.user_id, second.user_id);
        assert_eq!(first.occurred_at, second.occurred_at);
    }

    #[tokio::test]
    async fn stream_closes_on_revocation_and_never_replays_for_lagged_consumers() {
        let realtime = RealtimeService::connect(RedisService::disabled())
            .await
            .unwrap_or_else(|_| unreachable!());
        let sessions = Arc::new(Sessions(AtomicBool::new(false)));
        let stream = event_stream(
            realtime.subscribe(),
            sessions,
            Uuid::new_v4(),
            Uuid::new_v4(),
            unix_seconds() + 60,
            SsePolicy {
                heartbeat_interval: Duration::from_millis(10),
            },
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_closes_at_access_token_expiry() {
        let realtime = RealtimeService::connect(RedisService::disabled())
            .await
            .unwrap_or_else(|_| unreachable!());
        let stream = event_stream(
            realtime.subscribe(),
            Arc::new(Sessions(AtomicBool::new(true))),
            Uuid::new_v4(),
            Uuid::new_v4(),
            unix_seconds(),
            SsePolicy {
                heartbeat_interval: Duration::from_secs(25),
            },
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn slow_consumers_are_disconnected_when_the_bounded_buffer_lags() {
        let realtime = RealtimeService::connect(RedisService::disabled())
            .await
            .unwrap_or_else(|_| unreachable!());
        let user = Uuid::new_v4();
        let receiver = realtime.subscribe();
        for _ in 0..=BROADCAST_CAPACITY {
            realtime
                .emit(&[user], MobileEventType::MatchesInvalidated, Map::new())
                .await
                .unwrap_or_else(|_| unreachable!());
        }
        let stream = event_stream(
            receiver,
            Arc::new(Sessions(AtomicBool::new(true))),
            user,
            Uuid::new_v4(),
            unix_seconds() + 60,
            SsePolicy {
                heartbeat_interval: Duration::from_secs(25),
            },
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn rejects_invalid_cross_instance_events_without_panicking() {
        assert!(parse_event("not-json").is_none());
        assert!(
            parse_event(
                r#"{"id":"","user_id":"x","type":"message.created","occurred_at":"x","data":{}}"#
            )
            .is_none()
        );
        assert!(
            parse_event(r#"{"id":"x","user_id":"x","type":"unknown","occurred_at":"x","data":{}}"#)
                .is_none()
        );
    }
}
