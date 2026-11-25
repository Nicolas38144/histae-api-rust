use super::{StoreError, StoreFuture, StoreNotification, StoreProvider, StoreSnapshot};
use crate::config::StoreBillingConfig;
use crate::infra::tls::read_certificate_bundle;
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const MAX_CA_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationSource {
    Purchase,
    Notification,
    Reconciliation,
}

pub trait StoreGateway: Send + Sync {
    fn verify(
        &self,
        provider: StoreProvider,
        reference: String,
        source: VerificationSource,
    ) -> StoreFuture<'_, StoreSnapshot>;
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
        let enabled = config.apple_enabled || config.google_enabled;
        if config.production
            && (config.verifier_url.scheme() != "https"
                || (enabled && config.verifier_ca_file.is_none()))
        {
            return Err(StoreError::Unavailable);
        }
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20));
        if let Some(path) = config.verifier_ca_file.as_ref().filter(|_| enabled) {
            let pem =
                read_certificate_bundle(path, MAX_CA_BYTES).map_err(|_| StoreError::Unavailable)?;
            let roots =
                reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| StoreError::Unavailable)?;
            if roots.is_empty() {
                return Err(StoreError::Unavailable);
            }
            builder = builder.https_only(true).tls_certs_only(roots);
        }
        let client = builder.build().map_err(|_| StoreError::Unavailable)?;
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
    fn verify(
        &self,
        provider: StoreProvider,
        reference: String,
        source: VerificationSource,
    ) -> StoreFuture<'_, StoreSnapshot> {
        Box::pin(async move {
            let body = json!({"provider":provider,"reference":reference});
            match source {
                // The worker chooses this only for persisted, authenticated events.
                VerificationSource::Notification => {
                    self.call(&format!("verify-notification/{}", provider.as_str()), body)
                        .await
                }
                VerificationSource::Purchase | VerificationSource::Reconciliation => {
                    self.call("verify", body).await
                }
            }
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
                &format!("notification/{}", provider.as_str()),
                json!({"body":body,"authorization":authorization}),
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
