use super::{
    ReconciliationGatewayFuture, StripeCollection, StripeCustomerState, StripeReconciliationGateway,
};
use crate::billing::stripe::{StripeClient, StripeError};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::Value;
use uuid::Uuid;
impl StripeReconciliationGateway for StripeClient {
    fn retrieve_customer(
        &self,
        customer_id: &str,
    ) -> ReconciliationGatewayFuture<'_, StripeCustomerState> {
        let path = format!("customers/{customer_id}");
        Box::pin(async move {
            let value = self.request(Method::GET, &path, Vec::new(), None).await?;
            parse_customer(&value)
        })
    }

    fn list_customer_subscriptions(
        &self,
        customer_id: &str,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<Value>> {
        let customer_id = customer_id.to_owned();
        Box::pin(async move {
            let value = self
                .request(
                    Method::GET,
                    "subscriptions",
                    vec![
                        ("customer".to_owned(), customer_id),
                        ("status".to_owned(), "all".to_owned()),
                        ("limit".to_owned(), "100".to_owned()),
                        (
                            "expand[]".to_owned(),
                            "data.items.data.price.product".to_owned(),
                        ),
                    ],
                    None,
                )
                .await?;
            parse_collection(&value, |item| Ok(item.clone()))
        })
    }

    fn search_customers_by_attempt(
        &self,
        attempt_id: Uuid,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<StripeCustomerState>> {
        Box::pin(async move {
            let query = format!(
                "metadata['histae_customer_attempt_id']:'{}'",
                attempt_id.hyphenated()
            );
            let value = self
                .request(
                    Method::GET,
                    "customers/search",
                    vec![
                        ("query".to_owned(), query),
                        ("limit".to_owned(), "100".to_owned()),
                    ],
                    None,
                )
                .await?;
            parse_collection(&value, parse_customer)
        })
    }

    fn list_customers_created_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> ReconciliationGatewayFuture<'_, StripeCollection<StripeCustomerState>> {
        Box::pin(async move {
            let value = self
                .request(
                    Method::GET,
                    "customers",
                    vec![
                        ("created[gte]".to_owned(), start.timestamp().to_string()),
                        ("created[lte]".to_owned(), end.timestamp().to_string()),
                        ("limit".to_owned(), "100".to_owned()),
                    ],
                    None,
                )
                .await?;
            parse_collection(&value, parse_customer)
        })
    }
}

fn parse_collection<T, F>(value: &Value, mapper: F) -> Result<StripeCollection<T>, StripeError>
where
    F: Fn(&Value) -> Result<T, StripeError>,
{
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(StripeError::InvalidResponse)?;
    let items = data.iter().map(mapper).collect::<Result<Vec<_>, _>>()?;
    let truncated = value
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or(StripeError::InvalidResponse)?;
    Ok(StripeCollection { items, truncated })
}

fn parse_customer(value: &Value) -> Result<StripeCustomerState, StripeError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("cus_") && value.len() <= 255)
        .ok_or(StripeError::InvalidResponse)?;
    let metadata_user_id = value
        .pointer("/metadata/histae_user_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    let metadata_attempt_id = value
        .pointer("/metadata/histae_customer_attempt_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    Ok(StripeCustomerState {
        id: id.to_owned(),
        deleted: value
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        metadata_user_id,
        metadata_attempt_id,
    })
}
