use super::{ChallengeView, WebAccountError, WebAccountStore, WebIdentity};
use crate::{
    config::SecretString,
    identity::mobile::otp::{OtpError, OtpService},
    localization::Language,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct WebAccountService {
    store: Arc<dyn WebAccountStore>,
    otp: OtpService,
}
impl WebAccountService {
    pub fn new(store: impl WebAccountStore + 'static, otp: OtpService) -> Self {
        Self {
            store: Arc::new(store),
            otp,
        }
    }
    pub fn phone_key(&self, phone: &str) -> Result<String, WebAccountError> {
        self.otp
            .rate_limit_key(phone, OtpError::InvalidPhone)
            .map_err(|_| WebAccountError::InvalidRequest)
    }
    pub async fn start(
        &self,
        key: Uuid,
        user: Uuid,
        phone: &str,
        language: Language,
    ) -> Result<ChallengeView, WebAccountError> {
        let hash = self.phone_key(phone)?;
        let challenge = self
            .store
            .challenge(key, user, &hash, language.as_str())
            .await?
            .ok_or(WebAccountError::IdempotencyConflict)?;
        Ok(ChallengeView {
            challenge_id: challenge.id,
            expires_at: challenge.expires_at,
            message: "If the account details match, a verification code will be sent.",
        })
    }
    pub async fn verify(
        &self,
        challenge: Uuid,
        phone: &str,
        code: &str,
    ) -> Result<SecretString, WebAccountError> {
        let hash = self
            .phone_key(phone)
            .map_err(|_| WebAccountError::Authentication)?;
        let user = self
            .store
            .begin_verify(challenge, &hash)
            .await?
            .ok_or(WebAccountError::Authentication)?;
        self.otp
            .for_web_challenge(challenge)
            .consume(phone, code)
            .await
            .map_err(|_| WebAccountError::Authentication)?;
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| WebAccountError::Internal)?;
        let token = URL_SAFE_NO_PAD.encode(random);
        if !self.store.open_session(challenge, user, &token).await? {
            return Err(WebAccountError::Authentication);
        }
        Ok(SecretString::new(token))
    }
    pub async fn identity(&self, token: &str) -> Result<WebIdentity, WebAccountError> {
        self.store
            .identity(token)
            .await?
            .ok_or(WebAccountError::Authentication)
    }
    pub async fn logout(&self, token: &str) -> Result<(), WebAccountError> {
        self.store.logout(token).await.map_err(Into::into)
    }
}
