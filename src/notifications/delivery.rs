mod pg;
pub use pg::PgNotificationDeliveryStore;

use crate::localization::Language;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::domain::NotificationType;
use super::push::{PushDeliveryError, PushMessage, PushOutcome, PushSender};
use super::sse::{MobileEventType, RealtimeService};
use crate::infra::postgres::DatabaseError;
use crate::matches::domain::{PublicMatch, PublicMessage};
use crate::matches::service::{MatchEventFuture, MatchEventPublisher, MatchUpdate};
use crate::outbox::types::{DispatchFailure, DispatchOutcome, OutboxEvent};
use crate::outbox::worker::{DispatchFuture, OutboxHandler};

#[derive(Clone)]
pub struct PendingPush {
    pub notification_id: Uuid,
    pub device_id: Uuid,
    pub language: Language,
    pub token: String,
    pub notification_type: NotificationType,
    pub partner_firstname: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub payload: serde_json::Map<String, Value>,
}

pub type DeliveryStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait NotificationDeliveryStore: Send + Sync {
    fn find_deliverable(&self, id: Uuid) -> DeliveryStoreFuture<'_, Option<PendingPush>>;
    fn remove_registration<'a>(
        &'a self,
        device_id: Uuid,
        token: &'a str,
    ) -> DeliveryStoreFuture<'a, ()>;
}

#[derive(Clone)]
pub struct NotificationPushHandler {
    store: Arc<dyn NotificationDeliveryStore>,
    push: Arc<dyn PushSender>,
}

impl NotificationPushHandler {
    pub fn new(store: Arc<dyn NotificationDeliveryStore>, push: Arc<dyn PushSender>) -> Self {
        Self { store, push }
    }

    pub async fn deliver(&self, id: Uuid) -> Result<(), PushDeliveryError> {
        let Some(pending) = self
            .store
            .find_deliverable(id)
            .await
            .map_err(|_| PushDeliveryError)?
        else {
            return Ok(());
        };
        let outcome = self
            .push
            .send(PushMessage {
                token: &pending.token,
                kind: pending.notification_type,
                language: pending.language,
                firstname: pending.partner_firstname.as_deref(),
                expires_at: pending.expires_at,
                data: allowlisted_data(&pending)?,
            })
            .await?;
        if outcome == PushOutcome::Unregistered {
            self.store
                .remove_registration(pending.device_id, &pending.token)
                .await
                .map_err(|_| PushDeliveryError)?;
        }
        Ok(())
    }
}

impl OutboxHandler for NotificationPushHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(async move {
            self.deliver(event.aggregate_id)
                .await
                .map(|()| DispatchOutcome::Completed)
                .map_err(|_| DispatchFailure::transient("push_delivery_unavailable"))
        })
    }
}

fn allowlisted_data(pending: &PendingPush) -> Result<BTreeMap<String, String>, PushDeliveryError> {
    let mut data = BTreeMap::from([(
        "notification_id".to_owned(),
        pending.notification_id.hyphenated().to_string(),
    )]);
    match pending.notification_type {
        NotificationType::NewMatch
        | NotificationType::NewMessage
        | NotificationType::MatchExpiring
        | NotificationType::ContinuationRequested
        | NotificationType::ConversationContinued
        | NotificationType::PhotosRevealed
        | NotificationType::FirstPeriodEnding => {
            data.insert(
                "match_id".to_owned(),
                payload_string(&pending.payload, "match_id")?,
            );
        }
        NotificationType::DataRequestReplied => {
            data.insert(
                "request_id".to_owned(),
                payload_string(&pending.payload, "request_id")?,
            );
            if pending.payload.contains_key("request_message_id") {
                data.insert(
                    "request_message_id".to_owned(),
                    payload_string(&pending.payload, "request_message_id")?,
                );
            } else if pending.payload.contains_key("grant_id") {
                data.insert("request_event".to_owned(), "export_ready".to_owned());
            }
        }
        NotificationType::SupportCaseUpdated => {
            data.insert(
                "support_case_id".to_owned(),
                payload_string(&pending.payload, "support_case_id")?,
            );
        }
        NotificationType::NewSession => {}
    }
    if pending.notification_type == NotificationType::NewMessage {
        data.insert(
            "message_id".to_owned(),
            payload_string(&pending.payload, "message_id")?,
        );
        data.insert(
            "sender_id".to_owned(),
            payload_string(&pending.payload, "sender_id")?,
        );
    }
    Ok(data)
}

fn payload_string(
    payload: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<String, PushDeliveryError> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(PushDeliveryError)
}

#[derive(Clone)]
pub struct MobileDeliveryService {
    realtime: RealtimeService,
}

impl MobileDeliveryService {
    pub fn new(realtime: RealtimeService) -> Self {
        Self { realtime }
    }

    pub async fn matches_invalidated(&self, user_ids: [Uuid; 2]) {
        self.deliver(
            &user_ids,
            MobileEventType::MatchesInvalidated,
            serde_json::Map::new(),
        )
        .await;
    }

    async fn deliver(
        &self,
        user_ids: &[Uuid],
        kind: MobileEventType,
        data: serde_json::Map<String, Value>,
    ) {
        if self.realtime.emit(user_ids, kind, data).await.is_err() {
            tracing::warn!(event_code = "realtime_delivery_failed");
        }
    }
}

impl MatchEventPublisher for MobileDeliveryService {
    fn created<'a>(&'a self, item: &'a PublicMatch) -> MatchEventFuture<'a> {
        Box::pin(async move {
            self.deliver(
                &[item.user1_id, item.user2_id],
                MobileEventType::MatchCreated,
                serde_json::Map::from_iter([("match_id".to_owned(), json_value(item.id))]),
            )
            .await;
        })
    }

    fn updated<'a>(
        &'a self,
        match_id: Uuid,
        participants: [Uuid; 2],
        update: MatchUpdate,
    ) -> MatchEventFuture<'a> {
        Box::pin(async move {
            let (name, value) = match update {
                MatchUpdate::PhotosRevealed(value) => ("photos_revealed", Value::Bool(value)),
                MatchUpdate::MatchConfirmed(value) => ("match_confirmed", Value::Bool(value)),
                MatchUpdate::Ended => ("status", Value::String("ended".to_owned())),
            };
            self.deliver(
                &participants,
                MobileEventType::MatchUpdated,
                serde_json::Map::from_iter([
                    ("match_id".to_owned(), json_value(match_id)),
                    (name.to_owned(), value),
                ]),
            )
            .await;
        })
    }

    fn message_created<'a>(
        &'a self,
        message: &'a PublicMessage,
        participants: [Uuid; 2],
    ) -> MatchEventFuture<'a> {
        Box::pin(async move {
            self.deliver(
                &participants,
                MobileEventType::MessageCreated,
                serde_json::Map::from_iter([
                    ("match_id".to_owned(), json_value(message.match_id)),
                    ("message_id".to_owned(), json_value(message.id)),
                    ("sender_id".to_owned(), json_value(message.sender_id)),
                ]),
            )
            .await;
        })
    }

    fn messages_read<'a>(
        &'a self,
        match_id: Uuid,
        participants: [Uuid; 2],
        reader_id: Uuid,
        read_through_message_id: Uuid,
    ) -> MatchEventFuture<'a> {
        Box::pin(async move {
            self.deliver(
                &participants,
                MobileEventType::MessageRead,
                serde_json::Map::from_iter([
                    ("match_id".to_owned(), json_value(match_id)),
                    ("read_by".to_owned(), json_value(reader_id)),
                    (
                        "read_through_message_id".to_owned(),
                        json_value(read_through_message_id),
                    ),
                ]),
            )
            .await;
        })
    }
}

impl crate::billing::stores::StoreEvents for MobileDeliveryService {
    fn subscription_changed(&self, user_id: Uuid) -> crate::billing::stores::StoreFuture<'_, ()> {
        Box::pin(async move {
            self.deliver(
                &[user_id],
                MobileEventType::SubscriptionUpdated,
                serde_json::Map::new(),
            )
            .await;
            Ok(())
        })
    }
}

fn json_value(id: Uuid) -> Value {
    Value::String(id.hyphenated().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Store {
        pending: Option<PendingPush>,
        removed: Mutex<Vec<Uuid>>,
    }

    impl NotificationDeliveryStore for Store {
        fn find_deliverable(&self, _id: Uuid) -> DeliveryStoreFuture<'_, Option<PendingPush>> {
            let pending = self.pending.clone();
            Box::pin(async move { Ok(pending) })
        }

        fn remove_registration<'a>(
            &'a self,
            device_id: Uuid,
            token: &'a str,
        ) -> DeliveryStoreFuture<'a, ()> {
            Box::pin(async move {
                if !self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.device_id == device_id && pending.token == token)
                {
                    return Err(DatabaseError::QueryFailed);
                }
                self.removed
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(device_id);
                Ok(())
            })
        }
    }

    #[derive(Default)]
    struct Sender {
        calls: Mutex<Vec<BTreeMap<String, String>>>,
    }

    impl PushSender for Sender {
        fn send<'a>(&'a self, message: PushMessage<'a>) -> super::super::push::PushFuture<'a> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .map_err(|_| PushDeliveryError)?
                    .push(message.data);
                Ok(PushOutcome::Unregistered)
            })
        }
    }

    #[test]
    fn casework_pushes_forward_only_routing_fields() {
        use serde_json::json;

        let notification_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let case_id = Uuid::new_v4();
        for (kind, payload, expected) in [
            (
                NotificationType::DataRequestReplied,
                json!({"request_id": request_id, "request_status": "in_progress"}),
                vec![("request_id", request_id.to_string())],
            ),
            (
                NotificationType::DataRequestReplied,
                json!({"request_id": request_id, "request_message_id": message_id}),
                vec![
                    ("request_id", request_id.to_string()),
                    ("request_message_id", message_id.to_string()),
                ],
            ),
            (
                NotificationType::DataRequestReplied,
                json!({"request_id": request_id, "grant_id": Uuid::new_v4()}),
                vec![
                    ("request_id", request_id.to_string()),
                    ("request_event", "export_ready".to_owned()),
                ],
            ),
            (
                NotificationType::SupportCaseUpdated,
                json!({"support_case_id": case_id, "updated_at": Utc::now()}),
                vec![("support_case_id", case_id.to_string())],
            ),
        ] {
            let mut payload = payload.as_object().expect("object payload").clone();
            payload.insert("body".into(), json!("Synthetic user message"));
            payload.insert("response".into(), json!("Synthetic team reply"));
            let pending = PendingPush {
                notification_id,
                device_id: Uuid::new_v4(),
                language: Language::Fr,
                token: "synthetic-device-token".into(),
                notification_type: kind,
                partner_firstname: None,
                expires_at: Utc::now(),
                payload,
            };
            let expected = expected
                .into_iter()
                .chain([("notification_id", notification_id.to_string())])
                .map(|(key, value)| (key.to_owned(), value))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(allowlisted_data(&pending).expect("routing data"), expected);
        }
    }

    #[tokio::test]
    async fn forwards_public_metadata_and_removes_the_unregistered_device() {
        let notification_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let mut payload = serde_json::Map::new();
        payload.insert(
            "match_id".to_owned(),
            Value::String(Uuid::new_v4().to_string()),
        );
        payload.insert(
            "message_id".to_owned(),
            Value::String(Uuid::new_v4().to_string()),
        );
        payload.insert(
            "sender_id".to_owned(),
            Value::String(Uuid::new_v4().to_string()),
        );
        payload.insert("content".to_owned(), Value::String("private".to_owned()));
        let sender = Arc::new(Sender::default());
        let store = Arc::new(Store {
            pending: Some(PendingPush {
                device_id,
                language: crate::localization::Language::Fr,
                notification_id,
                token: "generated-device-token".to_owned(),
                notification_type: NotificationType::NewMessage,
                partner_firstname: None,
                expires_at: Utc::now() + chrono::Duration::minutes(10),
                payload,
            }),
            removed: Mutex::default(),
        });
        let handler = NotificationPushHandler::new(store.clone(), sender.clone());
        handler
            .deliver(Uuid::new_v4())
            .await
            .unwrap_or_else(|_| unreachable!());
        let calls = sender.calls.lock().unwrap_or_else(|_| unreachable!());
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("notification_id"),
            Some(&notification_id.to_string())
        );
        assert!(!calls[0].contains_key("content"));
        assert_eq!(
            *store.removed.lock().expect("removed registrations"),
            [device_id]
        );
    }

    #[tokio::test]
    async fn acknowledges_missing_or_ineligible_deliveries_without_network() {
        let sender = Arc::new(Sender::default());
        let handler = NotificationPushHandler::new(
            Arc::new(Store {
                pending: None,
                removed: Mutex::default(),
            }),
            sender.clone(),
        );
        handler
            .deliver(Uuid::new_v4())
            .await
            .unwrap_or_else(|_| unreachable!());
        assert!(
            sender
                .calls
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .is_empty()
        );
    }
}
