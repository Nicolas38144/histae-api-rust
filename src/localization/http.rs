use super::{Language, LocalizationService, catalog::CatalogSnapshot};
use crate::{
    config::LimitPolicy,
    http::{
        ApiError,
        extract::{ApiDto, ValidatedJson},
        rate_limit::RateLimiter,
        router::HttpState,
    },
    identity::mobile::http::{AuthenticatedMobile, MobileAuthState},
};
use axum::{
    Json, Router,
    extract::{Extension, FromRequestParts, Request},
    http::{HeaderValue, StatusCode, header, request::Parts},
    middleware::Next,
    response::Response,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

#[derive(Clone, Default)]
struct ResponseLanguage(Arc<OnceLock<Language>>);

/// Carries request metadata only; account access is resolved after authentication.
pub struct Locale {
    pub requested: Option<Language>,
    service: Option<LocalizationService>,
    report: ResponseLanguage,
}
impl FromRequestParts<HttpState> for Locale {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &HttpState) -> Result<Self, ApiError> {
        Ok(Self {
            requested: parts
                .headers
                .get(header::ACCEPT_LANGUAGE)
                .map(|value| Language::negotiate(value.to_str().unwrap_or_default())),
            service: state.localization.clone(),
            report: parts
                .extensions
                .get::<ResponseLanguage>()
                .cloned()
                .unwrap_or_default(),
        })
    }
}
pub struct ResolvedLocale {
    pub language: Language,
    service: Option<LocalizationService>,
}
impl Locale {
    /// Reuse the account projection loaded by the session guard, with no extra SQL query.
    pub fn for_account(
        self,
        account: &crate::identity::mobile::domain::ActiveAccount,
    ) -> ResolvedLocale {
        let language = self.requested.unwrap_or(account.preferred_language);
        let _ = self.report.0.set(language);
        ResolvedLocale {
            language,
            service: self.service,
        }
    }
    pub async fn resolve(self, user_id: Uuid) -> Result<ResolvedLocale, ApiError> {
        let language = match (self.requested, &self.service) {
            (Some(language), _) => language,
            (None, Some(service)) => service.language(user_id).await.map_err(unavailable)?,
            _ => Language::Fr,
        };
        let _ = self.report.0.set(language);
        Ok(ResolvedLocale {
            language,
            service: self.service,
        })
    }
    pub fn anonymous(self) -> ResolvedLocale {
        let language = self.requested.unwrap_or_default();
        let _ = self.report.0.set(language);
        ResolvedLocale {
            language,
            service: self.service,
        }
    }
}
impl ResolvedLocale {
    pub async fn catalog(&self, fresh: bool) -> Result<Arc<CatalogSnapshot>, ApiError> {
        match &self.service {
            Some(service) => service.catalog(fresh).await.map_err(unavailable),
            None if self.language == Language::Fr => Ok(Arc::new(CatalogSnapshot::default())),
            None => Err(unavailable(
                crate::infra::postgres::DatabaseError::QueryFailed,
            )),
        }
    }
    pub async fn invalidate(&self) {
        if let Some(service) = &self.service {
            service.invalidate().await;
        }
    }
}
pub fn unavailable(_: crate::infra::postgres::DatabaseError) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "localization_unavailable",
        "The language settings are temporarily unavailable.",
    )
}
pub async fn response_language(mut request: Request, next: Next) -> Response {
    let report = ResponseLanguage::default();
    request.extensions_mut().insert(report.clone());
    let mut response = next.run(request).await;
    if let Some(language) = report.0.get() {
        response.headers_mut().insert(
            header::CONTENT_LANGUAGE,
            HeaderValue::from_static(language.as_str()),
        );
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Accept-Language"));
    }
    response
}

#[derive(Clone)]
pub struct LanguageHttpState {
    pub service: LocalizationService,
    pub limiter: RateLimiter,
    pub policy: LimitPolicy,
}
pub fn routes(state: LanguageHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/language", get(read).patch(write))
        .layer(Extension(state))
        .layer(Extension(auth))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LanguageBody {
    language: Language,
}
impl ApiDto for LanguageBody {
    const ERROR_CODE: &'static str = "invalid_language";
    const ERROR_MESSAGE: &'static str = "The language must be fr, en, es or it.";
    fn is_valid(&self) -> bool {
        true
    }
}
async fn read(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<LanguageHttpState>,
    Extension(report): Extension<ResponseLanguage>,
) -> Result<Json<LanguageBody>, ApiError> {
    state
        .limiter
        .enforce(
            "language-settings",
            &identity.account.user_id.to_string(),
            &state.policy,
            "language_settings_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .language(identity.account.user_id)
        .await
        .map(|language| {
            let _ = report.0.set(language);
            Json(LanguageBody { language })
        })
        .map_err(unavailable)
}
async fn write(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<LanguageHttpState>,
    Extension(report): Extension<ResponseLanguage>,
    ValidatedJson(body): ValidatedJson<LanguageBody>,
) -> Result<Json<LanguageBody>, ApiError> {
    state
        .limiter
        .enforce(
            "language-settings",
            &identity.account.user_id.to_string(),
            &state.policy,
            "language_settings_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .set_language(identity.account.user_id, body.language)
        .await
        .map(|language| {
            let _ = report.0.set(language);
            Json(LanguageBody { language })
        })
        .map_err(unavailable)
}
