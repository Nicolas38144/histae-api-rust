#![cfg(feature = "redis-integration")]

use std::time::Duration;

use histae_api_rust::config::{RedisConfig, SecretString};
use histae_api_rust::infra::redis::RedisService;
use tokio::sync::mpsc;
use tokio::time;
use uuid::Uuid;

fn test_config() -> RedisConfig {
    RedisConfig {
        address: std::env::var("HISTAE_TEST_REDIS_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:6379".to_owned()),
        password: SecretString::new(
            std::env::var("HISTAE_TEST_REDIS_PASSWORD").unwrap_or_default(),
        ),
        db: 15,
        tls: false,
        connect_timeout: Duration::from_secs(2),
        command_timeout: Duration::from_secs(1),
        root_certificate: None,
    }
}

fn assert_loopback_target(address: &str) {
    let host = address.rsplit_once(':').map(|(host, _)| host);
    assert!(
        matches!(host, Some("127.0.0.1" | "localhost" | "::1")),
        "Redis integration tests refuse non-loopback targets"
    );
}

#[tokio::test]
async fn fixed_window_health_and_pubsub_work_against_real_redis() {
    let config = test_config();
    assert_loopback_target(&config.address);
    let redis = RedisService::connect(&config, true)
        .await
        .expect("connect to the dedicated local Redis test database");
    redis.check().await.expect("Redis PING");

    let unique = Uuid::new_v4();
    let key = format!("histae:test:s07:window:{unique}");
    let first = redis
        .increment_fixed_window(&key, Duration::from_secs(2))
        .await
        .expect("first fixed-window increment");
    let second = redis
        .increment_fixed_window(&key, Duration::from_secs(2))
        .await
        .expect("second fixed-window increment");
    assert_eq!(first.count, 1);
    assert_eq!(second.count, 2);
    assert!((1..=2_000).contains(&first.ttl_millis));
    assert!((1..=first.ttl_millis).contains(&second.ttl_millis));

    let channel = format!("histae:test:s07:channel:{unique}");
    let (sender, mut receiver) = mpsc::channel(4);
    let subscription = redis
        .subscribe(channel.clone(), sender)
        .await
        .expect("subscribe with a dedicated Redis connection");
    redis
        .publish(&channel, "contract-message")
        .await
        .expect("publish through the shared connection manager");
    let message = time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("Pub/Sub delivery timeout")
        .expect("subscription remained open");
    assert_eq!(message, "contract-message");
    subscription.close().await;
}
