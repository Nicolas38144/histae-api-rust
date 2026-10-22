use super::{ChallengeView, WebBillingError, WebBillingStore, WebIdentity, WebSessionView};
use crate::{
    billing::{
        domain::{BillingPeriod, CheckoutSessionView},
        service::BillingService,
    },
    config::SecretString,
    identity::mobile::otp::{OtpError, OtpService},
    localization::Language,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct WebBillingService {
    store: Arc<dyn WebBillingStore>,
    otp: OtpService,
    billing: Arc<BillingService>,
}
impl WebBillingService {
    pub fn new(
        store: impl WebBillingStore + 'static,
        otp: OtpService,
        billing: Arc<BillingService>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            otp,
            billing,
        }
    }
    pub fn phone_key(&self, phone: &str) -> Result<String, WebBillingError> {
        self.otp
            .rate_limit_key(phone, OtpError::InvalidPhone)
            .map_err(|_| WebBillingError::InvalidRequest)
    }
    pub async fn start(
        &self,
        key: Uuid,
        user: Uuid,
        phone: &str,
        language: Language,
    ) -> Result<ChallengeView, WebBillingError> {
        let hash = self.phone_key(phone)?;
        let challenge = self
            .store
            .challenge(key, user, &hash, language.as_str())
            .await?
            .ok_or(WebBillingError::IdempotencyConflict)?;
        if challenge.created && challenge.matched {
            // The OTP service owns durable Sweego limits. Keep failures generic
            // to avoid disclosing whether the UUID/phone pair exists.
            let _ = self
                .otp
                .for_web_challenge(challenge.id)
                .send_in(phone, Some(&challenge.id.to_string()), language)
                .await;
        }
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
    ) -> Result<SecretString, WebBillingError> {
        let hash = self
            .phone_key(phone)
            .map_err(|_| WebBillingError::Authentication)?;
        let user = self
            .store
            .begin_verify(challenge, &hash)
            .await?
            .ok_or(WebBillingError::Authentication)?;
        self.otp
            .for_web_challenge(challenge)
            .consume(phone, code)
            .await
            .map_err(|_| WebBillingError::Authentication)?;
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| WebBillingError::Internal)?;
        let token = URL_SAFE_NO_PAD.encode(random);
        if !self.store.open_session(challenge, user, &token).await? {
            return Err(WebBillingError::Authentication);
        }
        Ok(SecretString::new(token))
    }
    async fn identity(&self, token: &str) -> Result<WebIdentity, WebBillingError> {
        self.store
            .identity(token)
            .await?
            .ok_or(WebBillingError::Authentication)
    }
    pub async fn session(&self, token: &str) -> Result<WebSessionView, WebBillingError> {
        let identity = self.identity(token).await?;
        let subscription = self.billing.subscription(identity.user_id).await?;
        Ok(WebSessionView {
            user_id: identity.user_id,
            onboarding_complete: identity.onboarding_complete,
            subscription,
        })
    }
    pub async fn payer(&self, token: &str) -> Result<Uuid, WebBillingError> {
        let identity = self.identity(token).await?;
        if !identity.onboarding_complete {
            return Err(WebBillingError::OnboardingIncomplete);
        }
        Ok(identity.user_id)
    }
    pub async fn logout(&self, token: &str) -> Result<(), WebBillingError> {
        self.store.logout(token).await.map_err(Into::into)
    }
    pub async fn checkout(
        &self,
        user: Uuid,
        period: BillingPeriod,
        key: Option<&str>,
        language: Language,
    ) -> Result<CheckoutSessionView, WebBillingError> {
        self.billing
            .create_checkout_in(user, period, key, language)
            .await
            .map_err(Into::into)
    }
    pub async fn portal(&self, user: Uuid, language: Language) -> Result<String, WebBillingError> {
        self.billing
            .create_portal_in(user, language)
            .await
            .map_err(Into::into)
    }
}
