use std::fmt;
use std::fs;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{
    Client, ConnectionAddr, ConnectionInfo, ProtocolVersion, RedisConnectionInfo, TlsCertificates,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::config::RedisConfig;

const FIXED_WINDOW_SCRIPT: &str = "local current = redis.call('INCR', KEYS[1]); if current == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]); end; return {current, redis.call('PTTL', KEYS[1])}";
const MAX_ROOT_CERTIFICATE_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedisError {
    Disabled,
    InvalidConfiguration,
    ConnectionFailed,
    CommandTimedOut,
    CommandFailed,
    SubscriptionFailed,
}

impl RedisError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::Disabled => "redis_disabled",
            Self::InvalidConfiguration => "redis_invalid_configuration",
            Self::ConnectionFailed => "redis_connection_failed",
            Self::CommandTimedOut => "redis_command_timed_out",
            Self::CommandFailed => "redis_command_failed",
            Self::SubscriptionFailed => "redis_subscription_failed",
        }
    }
}

impl fmt::Display for RedisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_code())
    }
}

impl std::error::Error for RedisError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedWindowIncrement {
    pub count: u64,
    pub ttl_millis: i64,
}

#[derive(Clone)]
pub struct RedisService {
    inner: Option<Arc<RedisInner>>,
}

struct RedisInner {
    client: Client,
    manager: ConnectionManager,
    command_timeout: Duration,
}

impl RedisService {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub async fn connect(config: &RedisConfig, enabled: bool) -> Result<Self, RedisError> {
        if !enabled {
            return Ok(Self::disabled());
        }
        let client = build_client(config)?;
        let manager_config = ConnectionManagerConfig::new()
            .set_factor(100)
            .set_exponent_base(2)
            .set_max_delay(2_000)
            .set_number_of_retries(5)
            .set_connection_timeout(config.connect_timeout)
            .set_response_timeout(config.command_timeout);
        let manager = time::timeout(
            config.connect_timeout,
            ConnectionManager::new_with_config(client.clone(), manager_config),
        )
        .await
        .map_err(|_| RedisError::ConnectionFailed)?
        .map_err(|_| RedisError::ConnectionFailed)?;
        Ok(Self {
            inner: Some(Arc::new(RedisInner {
                client,
                manager,
                command_timeout: config.command_timeout,
            })),
        })
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub async fn increment_fixed_window(
        &self,
        key: &str,
        window: Duration,
    ) -> Result<FixedWindowIncrement, RedisError> {
        let inner = self.inner()?;
        let window_millis = u64::try_from(window.as_millis()).unwrap_or(u64::MAX);
        let mut manager = inner.manager.clone();
        let result: (u64, i64) = command_with_timeout(inner, async move {
            redis::cmd("EVAL")
                .arg(FIXED_WINDOW_SCRIPT)
                .arg(1)
                .arg(key)
                .arg(window_millis)
                .query_async(&mut manager)
                .await
        })
        .await?;
        Ok(FixedWindowIncrement {
            count: result.0,
            ttl_millis: result.1,
        })
    }

    pub async fn check(&self) -> Result<(), RedisError> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        let mut manager = inner.manager.clone();
        let response: String = command_with_timeout(inner, async move {
            redis::cmd("PING").query_async(&mut manager).await
        })
        .await?;
        if response == "PONG" {
            Ok(())
        } else {
            Err(RedisError::CommandFailed)
        }
    }

    pub async fn publish(&self, channel: &str, message: &str) -> Result<(), RedisError> {
        let inner = self.inner()?;
        let mut manager = inner.manager.clone();
        let _: u64 = command_with_timeout(inner, async move {
            redis::cmd("PUBLISH")
                .arg(channel)
                .arg(message)
                .query_async(&mut manager)
                .await
        })
        .await?;
        Ok(())
    }

    pub async fn subscribe(
        &self,
        channel: String,
        messages: mpsc::Sender<String>,
    ) -> Result<RedisSubscription, RedisError> {
        let inner = self.inner()?;
        let mut pubsub = time::timeout(inner.command_timeout, inner.client.get_async_pubsub())
            .await
            .map_err(|_| RedisError::CommandTimedOut)?
            .map_err(|_| RedisError::SubscriptionFailed)?;
        time::timeout(inner.command_timeout, pubsub.subscribe(channel.clone()))
            .await
            .map_err(|_| RedisError::CommandTimedOut)?
            .map_err(|_| RedisError::SubscriptionFailed)?;

        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let command_timeout = inner.command_timeout;
        let task = tokio::spawn(async move {
            let mut stream = pubsub.on_message();
            loop {
                tokio::select! {
                    _ = task_cancellation.cancelled() => break,
                    message = stream.next() => {
                        let Some(message) = message else { break; };
                        let Ok(payload) = message.get_payload::<String>() else {
                            tracing::warn!(event_code = "redis_subscriber_failed", error_code = "invalid_payload");
                            continue;
                        };
                        match messages.try_send(payload) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!(event_code = "redis_subscriber_failed", error_code = "subscriber_backpressure");
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                }
            }
            drop(stream);
            let _ = time::timeout(command_timeout, pubsub.unsubscribe(channel)).await;
        });
        Ok(RedisSubscription {
            cancellation,
            task: Some(task),
        })
    }

    fn inner(&self) -> Result<&RedisInner, RedisError> {
        self.inner.as_deref().ok_or(RedisError::Disabled)
    }
}

pub struct RedisSubscription {
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl RedisSubscription {
    pub async fn close(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for RedisSubscription {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.take();
    }
}

async fn command_with_timeout<T>(
    inner: &RedisInner,
    operation: impl Future<Output = redis::RedisResult<T>>,
) -> Result<T, RedisError> {
    time::timeout(inner.command_timeout, operation)
        .await
        .map_err(|_| RedisError::CommandTimedOut)?
        .map_err(|_| RedisError::CommandFailed)
}

fn build_client(config: &RedisConfig) -> Result<Client, RedisError> {
    let (host, port) = parse_address(&config.address)?;
    let connection_info = ConnectionInfo {
        addr: if config.tls {
            ConnectionAddr::TcpTls {
                host,
                port,
                insecure: false,
                tls_params: None,
            }
        } else {
            ConnectionAddr::Tcp(host, port)
        },
        redis: RedisConnectionInfo {
            db: i64::from(config.db),
            username: None,
            password: (!config.password.expose_secret().is_empty())
                .then(|| config.password.expose_secret().to_owned()),
            protocol: ProtocolVersion::RESP2,
        },
    };

    if config.tls {
        let root_cert = config
            .root_certificate
            .as_ref()
            .map(|path| {
                let metadata = fs::metadata(path).map_err(|_| RedisError::InvalidConfiguration)?;
                if metadata.len() > MAX_ROOT_CERTIFICATE_BYTES {
                    return Err(RedisError::InvalidConfiguration);
                }
                fs::read(path).map_err(|_| RedisError::InvalidConfiguration)
            })
            .transpose()?;
        Client::build_with_tls(
            connection_info,
            TlsCertificates {
                client_tls: None,
                root_cert,
            },
        )
        .map_err(|_| RedisError::InvalidConfiguration)
    } else {
        Client::open(connection_info).map_err(|_| RedisError::InvalidConfiguration)
    }
}

fn parse_address(address: &str) -> Result<(String, u16), RedisError> {
    let Some((host, port)) = address.rsplit_once(':') else {
        return Err(RedisError::InvalidConfiguration);
    };
    let port = port
        .parse::<u16>()
        .map_err(|_| RedisError::InvalidConfiguration)?;
    if host.is_empty() {
        return Err(RedisError::InvalidConfiguration);
    }
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_validated_host_and_port_without_accepting_partial_values() {
        assert_eq!(
            parse_address("redis.internal:6379"),
            Ok(("redis.internal".to_owned(), 6379))
        );
        assert_eq!(
            parse_address("redis.internal"),
            Err(RedisError::InvalidConfiguration)
        );
        assert_eq!(
            parse_address("redis.internal:70000"),
            Err(RedisError::InvalidConfiguration)
        );
    }

    #[tokio::test]
    async fn disabled_redis_is_ready_but_refuses_data_operations() {
        let redis = RedisService::disabled();
        assert_eq!(redis.check().await, Ok(()));
        assert_eq!(
            redis
                .increment_fixed_window("key", Duration::from_secs(1))
                .await,
            Err(RedisError::Disabled)
        );
    }
}
