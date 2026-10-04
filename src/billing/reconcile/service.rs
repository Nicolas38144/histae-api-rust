use super::{
    BillingReconciliationStore, CustomerCreationContext, CustomerRecoveryResult,
    ReconciliationApplyState, ReconciliationCursor, ReconciliationItem, ReconciliationKind,
    ReconciliationRow, ReconciliationStoreError, StripeReconciliationGateway, SubscriptionContext,
};
use crate::billing::{
    domain::CUSTOMER_CREATE_SAFETY_HOURS,
    webhook::{
        BillingRealtimePublisher, SubscriptionProjection, map_subscription, reconciliation_due,
    },
};
use crate::config::{BillingConfig, BillingProvider};
use crate::infra::{
    postgres::DatabaseError,
    postgres_locks::{AccountActivityError, AccountActivityPool},
};
use crate::outbox::types::OutboxEventType;
use crate::shared::clock::Clock;
use crate::shared::text::validator_js_length;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin, sync::Arc};
use uuid::{Uuid, Variant};
const LEGACY_CUSTOMER_WINDOW_MINUTES: i64 = 5;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BillingReconciliationError {
    pub code: &'static str,
    pub permanent: bool,
}

impl BillingReconciliationError {
    const fn transient(code: &'static str) -> Self {
        Self {
            code,
            permanent: false,
        }
    }

    const fn permanent(code: &'static str) -> Self {
        Self {
            code,
            permanent: true,
        }
    }
}

impl fmt::Display for BillingReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for BillingReconciliationError {}

impl From<AccountActivityError> for BillingReconciliationError {
    fn from(_: AccountActivityError) -> Self {
        Self::transient("billing_account_activity_unavailable")
    }
}

#[derive(Clone)]
pub struct BillingReconciliationService {
    store: Arc<dyn BillingReconciliationStore>,
    stripe: Arc<dyn StripeReconciliationGateway>,
    realtime: Arc<dyn BillingRealtimePublisher>,
    activity: Arc<AccountActivityPool>,
    clock: Arc<dyn Clock>,
    config: Arc<BillingConfig>,
}

impl BillingReconciliationService {
    pub fn new(
        store: Arc<dyn BillingReconciliationStore>,
        stripe: Arc<dyn StripeReconciliationGateway>,
        realtime: Arc<dyn BillingRealtimePublisher>,
        activity: Arc<AccountActivityPool>,
        clock: Arc<dyn Clock>,
        config: BillingConfig,
    ) -> Self {
        Self {
            store,
            stripe,
            realtime,
            activity,
            clock,
            config: Arc::new(config),
        }
    }

    pub async fn process(
        &self,
        event_type: &OutboxEventType,
        aggregate_id: Uuid,
    ) -> Result<(), BillingReconciliationError> {
        if self.config.provider != BillingProvider::Stripe {
            return Err(BillingReconciliationError::transient(
                "billing_reconciliation_disabled",
            ));
        }
        match event_type {
            OutboxEventType::BillingSubscriptionReconcile => {
                self.reconcile_subscription(aggregate_id).await
            }
            OutboxEventType::BillingCustomerReconcile => {
                self.reconcile_customer_creation(aggregate_id).await
            }
            _ => Err(BillingReconciliationError::permanent(
                "billing_reconciliation_event_invalid",
            )),
        }
    }

    async fn reconcile_subscription(
        &self,
        user_id: Uuid,
    ) -> Result<(), BillingReconciliationError> {
        let initial = self
            .store
            .subscription_context(user_id)
            .await
            .map_err(map_store_failure)?;
        if initial.is_none() {
            return Ok(());
        }
        let activity = Arc::clone(&self.activity);
        let service = self.clone();
        activity
            .run_existing(&[user_id], move |lease| {
                let service = service.clone();
                Box::pin(async move {
                    let context = service
                        .store
                        .subscription_context(user_id)
                        .await
                        .map_err(map_store_failure)?;
                    let Some(context) = context else {
                        return Ok(());
                    };
                    let snapshot_at = service.clock.now();
                    let customer = service
                        .stripe
                        .retrieve_customer(&context.stripe_customer_id)
                        .await
                        .map_err(|_| {
                            BillingReconciliationError::transient("billing_provider_unavailable")
                        })?;
                    lease.assert_held()?;
                    let projection = if customer.deleted {
                        None
                    } else {
                        service
                            .subscription_projection(&context, snapshot_at)
                            .await?
                    };
                    lease.assert_held()?;
                    let result = service
                        .store
                        .apply_subscription(
                            context.clone(),
                            projection,
                            snapshot_at,
                            reconciliation_due(
                                snapshot_at,
                                service.config.reconciliation_freshness,
                            ),
                            customer.deleted,
                        )
                        .await
                        .map_err(map_store_failure)?;
                    lease.assert_held()?;
                    if result.state == ReconciliationApplyState::Conflict {
                        return Err(BillingReconciliationError::permanent(
                            "billing_subscription_mapping_conflict",
                        ));
                    }
                    if result.state == ReconciliationApplyState::Applied
                        && result.status != result.previous_status
                        && let Some(status) = result.status
                    {
                        let _ = service
                            .realtime
                            .subscription_updated(context.user_id, status)
                            .await;
                    }
                    Ok(())
                })
            })
            .await
    }

    async fn subscription_projection(
        &self,
        context: &SubscriptionContext,
        snapshot_at: DateTime<Utc>,
    ) -> Result<Option<SubscriptionProjection>, BillingReconciliationError> {
        let listed = self
            .stripe
            .list_customer_subscriptions(&context.stripe_customer_id)
            .await
            .map_err(|_| BillingReconciliationError::transient("billing_provider_unavailable"))?;
        if listed.truncated {
            return Err(BillingReconciliationError::permanent(
                "billing_subscription_set_too_large",
            ));
        }
        let mut premium = listed
            .items
            .into_iter()
            .filter(|subscription| {
                subscription
                    .pointer("/items/data")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item.pointer("/price/product")
                                .and_then(object_id)
                                .is_some_and(|id| id == self.config.premium_product_id)
                        })
                    })
            })
            .collect::<Vec<_>>();
        let current = premium
            .iter()
            .filter(|subscription| {
                subscription
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| {
                        matches!(
                            status,
                            "incomplete" | "trialing" | "active" | "past_due" | "paused"
                        )
                    })
            })
            .collect::<Vec<_>>();
        if current.len() > 1 {
            return Err(BillingReconciliationError::permanent(
                "billing_multiple_current_subscriptions",
            ));
        }
        let selected = if let Some(selected) = current.first() {
            Some((*selected).clone())
        } else {
            premium.sort_by_key(|subscription| {
                std::cmp::Reverse(
                    subscription
                        .get("created")
                        .and_then(Value::as_i64)
                        .unwrap_or(i64::MIN),
                )
            });
            premium.into_iter().next()
        };
        let Some(selected) = selected else {
            return Ok(None);
        };
        let projection = map_subscription(&selected, snapshot_at, &self.config)
            .map_err(|_| BillingReconciliationError::permanent("billing_projection_invalid"))?;
        if projection.stripe_customer_id != context.stripe_customer_id
            || projection
                .metadata_user_id
                .is_some_and(|user_id| user_id != context.user_id)
        {
            return Err(BillingReconciliationError::permanent(
                "billing_subscription_mapping_conflict",
            ));
        }
        Ok(Some(projection))
    }

    async fn reconcile_customer_creation(
        &self,
        attempt_id: Uuid,
    ) -> Result<(), BillingReconciliationError> {
        let initial = self
            .store
            .customer_creation_context(attempt_id)
            .await
            .map_err(map_store_failure)?;
        let Some(initial) = initial else {
            return Ok(());
        };
        if customer_creation_resolved(&initial) {
            return Ok(());
        }
        if initial.created_customer_id.is_none()
            && self.clock.now() - initial.started_at
                < TimeDelta::hours(CUSTOMER_CREATE_SAFETY_HOURS)
        {
            return Err(BillingReconciliationError::transient(
                "billing_customer_not_due",
            ));
        }
        let activity = Arc::clone(&self.activity);
        let service = self.clone();
        activity
            .run_existing(&[initial.user_id], move |lease| {
                let service = service.clone();
                Box::pin(async move {
                    let context = service
                        .store
                        .customer_creation_context(attempt_id)
                        .await
                        .map_err(map_store_failure)?;
                    let Some(context) = context else {
                        return Ok(());
                    };
                    if customer_creation_resolved(&context) {
                        return Ok(());
                    }
                    let customer_id = match context.created_customer_id.clone() {
                        Some(customer_id) => Some(customer_id),
                        None => service.find_created_customer(&context).await?,
                    };
                    lease.assert_held()?;
                    let result = service
                        .store
                        .recover_customer_creation(attempt_id, customer_id)
                        .await
                        .map_err(map_store_failure)?;
                    lease.assert_held()?;
                    if result == CustomerRecoveryResult::Conflict {
                        return Err(BillingReconciliationError::permanent(
                            "billing_customer_mapping_conflict",
                        ));
                    }
                    Ok(())
                })
            })
            .await
    }

    async fn find_created_customer(
        &self,
        context: &CustomerCreationContext,
    ) -> Result<Option<String>, BillingReconciliationError> {
        let exact = self
            .stripe
            .search_customers_by_attempt(context.attempt_id)
            .await
            .map_err(|_| BillingReconciliationError::transient("billing_provider_unavailable"))?;
        if exact.truncated || exact.items.len() > 1 {
            return Err(BillingReconciliationError::permanent(
                "billing_customer_search_ambiguous",
            ));
        }
        if let Some(customer) = exact.items.first() {
            if customer.metadata_user_id != Some(context.user_id) {
                return Err(BillingReconciliationError::permanent(
                    "billing_customer_mapping_conflict",
                ));
            }
            return Ok(Some(customer.id.clone()));
        }
        let window = TimeDelta::minutes(LEGACY_CUSTOMER_WINDOW_MINUTES);
        let legacy = self
            .stripe
            .list_customers_created_between(
                context.started_at - window,
                context.started_at + window,
            )
            .await
            .map_err(|_| BillingReconciliationError::transient("billing_provider_unavailable"))?;
        let candidates = legacy
            .items
            .into_iter()
            .filter(|customer| {
                customer.metadata_user_id == Some(context.user_id)
                    && customer
                        .metadata_attempt_id
                        .is_none_or(|attempt_id| attempt_id == context.attempt_id)
            })
            .collect::<Vec<_>>();
        if legacy.truncated || candidates.len() > 1 {
            return Err(BillingReconciliationError::permanent(
                "billing_customer_search_ambiguous",
            ));
        }
        Ok(candidates.into_iter().next().map(|customer| customer.id))
    }

    pub async fn list_page(
        &self,
        kind: Option<ReconciliationKind>,
        limit: u32,
        raw_cursor: Option<&str>,
    ) -> Result<ReconciliationPage, ReconciliationListError> {
        let cursor = decode_cursor(raw_cursor)?;
        let rows = self
            .store
            .list(kind, limit.saturating_add(1), cursor)
            .await
            .map_err(|error| match error {
                ReconciliationStoreError::Database(error) => {
                    ReconciliationListError::Database(error)
                }
                ReconciliationStoreError::InvalidStoredData => {
                    ReconciliationListError::Database(DatabaseError::QueryFailed)
                }
            })?;
        let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        let mut rows = rows;
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let next_cursor = if has_more {
            rows.last()
                .map(|row| encode_cursor(row.dead_lettered_at, row.event_id))
                .transpose()?
        } else {
            None
        };
        Ok(ReconciliationPage {
            events: rows.into_iter().map(ReconciliationItem::from).collect(),
            next_cursor,
        })
    }
}

fn customer_creation_resolved(context: &CustomerCreationContext) -> bool {
    context.customer_erased_at.is_some()
        || context.created_customer_id.is_some()
            && context.created_customer_id == context.mapped_customer_id
}

fn object_id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(Value::as_str))
}

fn map_store_failure(error: ReconciliationStoreError) -> BillingReconciliationError {
    match error {
        ReconciliationStoreError::Database(_) => {
            BillingReconciliationError::transient("billing_database_unavailable")
        }
        ReconciliationStoreError::InvalidStoredData => {
            BillingReconciliationError::permanent("billing_projection_invalid")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationListError {
    InvalidCursor,
    Database(DatabaseError),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReconciliationPage {
    pub events: Vec<ReconciliationItem>,
    pub next_cursor: Option<String>,
}

impl From<ReconciliationRow> for ReconciliationItem {
    fn from(row: ReconciliationRow) -> Self {
        Self {
            event_id: row.event_id,
            user_id: row.user_id,
            kind: row.kind,
            attempts: row.attempts,
            last_error_code: row.last_error_code,
            created_at: row.created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            dead_lettered_at: row
                .dead_lettered_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(
    value: Option<&str>,
) -> Result<Option<ReconciliationCursor>, ReconciliationListError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if validator_js_length(value) > 512 {
        return Err(ReconciliationListError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ReconciliationListError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| ReconciliationListError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| ReconciliationListError::InvalidCursor)?;
    if id.get_variant() != Variant::RFC4122
        || id.hyphenated().to_string() != cursor.id.to_ascii_lowercase()
    {
        return Err(ReconciliationListError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| ReconciliationListError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(ReconciliationCursor { at, id }))
}

fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> Result<String, ReconciliationListError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_rfc3339_opts(SecondsFormat::Micros, true),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| ReconciliationListError::InvalidCursor)
}

pub type ReconciliationHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ReconciliationPage, ReconciliationListError>> + Send + 'a>>;

pub trait BillingReconciliationListing: Send + Sync {
    fn list(
        &self,
        kind: Option<ReconciliationKind>,
        limit: u32,
        cursor: Option<&str>,
    ) -> ReconciliationHttpFuture<'_>;
}

impl BillingReconciliationListing for BillingReconciliationService {
    fn list(
        &self,
        kind: Option<ReconciliationKind>,
        limit: u32,
        cursor: Option<&str>,
    ) -> ReconciliationHttpFuture<'_> {
        let cursor = cursor.map(str::to_owned);
        Box::pin(async move { self.list_page(kind, limit, cursor.as_deref()).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_round_trip_preserves_microseconds_and_generated_ids() {
        let id = Uuid::new_v4();
        let at = DateTime::parse_from_rfc3339("2030-01-01T00:00:00.123456Z")
            .unwrap_or_else(|_| unreachable!())
            .with_timezone(&Utc);
        let encoded = encode_cursor(at, id).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            decode_cursor(Some(&encoded)),
            Ok(Some(ReconciliationCursor { at, id }))
        );
        assert_eq!(
            decode_cursor(Some("not-a-cursor")),
            Err(ReconciliationListError::InvalidCursor)
        );
    }

    #[test]
    fn resolved_customer_attempt_requires_the_same_mapping() {
        let context = CustomerCreationContext {
            attempt_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            started_at: Utc::now(),
            created_customer_id: Some("cus_Generated".to_owned()),
            mapped_customer_id: Some("cus_Generated".to_owned()),
            customer_erased_at: None,
        };
        assert!(customer_creation_resolved(&context));
    }
}
