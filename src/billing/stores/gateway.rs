use super::{StoreError, StoreFuture, StoreNotification, StoreProvider, StoreSnapshot};
use crate::config::StoreBillingConfig;
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

pub trait StoreGateway: Send + Sync {
    fn verify(&self, provider: StoreProvider, reference: String) -> StoreFuture<'_, StoreSnapshot>;
    fn notification(
        &self,
        provider: StoreProvider,
        body: Value,
        authorization: Option<String>,
    ) -> StoreFuture<'_, StoreNotification>;
    fn acknowledge(&self, reference: String) -> StoreFuture<'_, ()>;
}

#[derive(Clone)]
pub struct StoreVerifier {
    config: StoreBillingConfig,
    client: reqwest::Client,
}
impl StoreVerifier {
    pub fn new(config: StoreBillingConfig) -> Result<Self, StoreError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| StoreError::Unavailable)?;
        Ok(Self { config, client })
    }
    async fn call<T: DeserializeOwned>(
        &self,
        path: &str,
        body: impl Serialize,
    ) -> Result<T, StoreError> {
        let response = self
            .client
            .post(
                self.config
                    .verifier_url
                    .join(path)
                    .map_err(|_| StoreError::Unavailable)?,
            )
            .bearer_auth(self.config.verifier_token.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if response.status().as_u16() == 422 {
            return Err(StoreError::Invalid);
        }
        if !response.status().is_success() {
            return Err(StoreError::Unavailable);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| StoreError::Unavailable)?;
            if bytes.len() + chunk.len() > 131072 {
                return Err(StoreError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Unavailable)
    }
}
impl StoreGateway for StoreVerifier {
    fn verify(&self, provider: StoreProvider, reference: String) -> StoreFuture<'_, StoreSnapshot> {
        Box::pin(async move {
            self.call("verify", json!({"provider":provider,"reference":reference}))
                .await
        })
    }
    fn notification(
        &self,
        provider: StoreProvider,
        body: Value,
        authorization: Option<String>,
    ) -> StoreFuture<'_, StoreNotification> {
        Box::pin(async move {
            self.call(
                "notification",
                json!({"provider":provider,"body":body,"authorization":authorization}),
            )
            .await
        })
    }
    fn acknowledge(&self, reference: String) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let _: Value = self
                .call(
                    "acknowledge",
                    json!({"provider":"google_play","reference":reference}),
                )
                .await?;
            Ok(())
        })
    }
}
