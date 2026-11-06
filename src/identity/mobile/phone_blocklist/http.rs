use super::{
    domain::{Change, Decision, Entry, Overview, PhoneKey, PolicyError, Target, canonical_phone},
    pg::PgPhoneBlocklist,
};
use crate::{
    config::SecretString,
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson, ValidatedQuery},
        router::HttpState,
    },
    identity::admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity},
    infra::postgres::Database,
};
use axum::{
    Json, Router,
    extract::Extension,
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone)]
struct State {
    store: PgPhoneBlocklist,
    key: SecretString,
}

pub fn routes(
    database: Database,
    key: SecretString,
    auth: AdminAuthHttpState,
) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/admin/sms-safety/phone-blocklist",
            get(overview).patch(change),
        )
        .route("/api/admin/sms-safety/phone-blocklist/lookup", post(lookup))
        .layer(Extension(State {
            store: PgPhoneBlocklist::new(database),
            key,
        }))
        .layer(Extension(auth))
}
#[derive(Deserialize)]
struct Page {
    limit: Option<u32>,
    offset: Option<u32>,
}
impl ApiDto for Page {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";
    fn is_valid(&self) -> bool {
        (1..=100).contains(&self.limit.unwrap_or(50)) && self.offset.unwrap_or(0) <= 100_000
    }
}
async fn overview(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<State>,
    ValidatedQuery(page): ValidatedQuery<Page>,
) -> Result<Json<Overview>, ApiError> {
    let limit = page.limit.unwrap_or(50);
    let offset = page.offset.unwrap_or(0);
    state
        .store
        .overview(limit, offset)
        .await
        .map(Json)
        .map_err(error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    phone_number: String,
}
impl ApiDto for Lookup {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";
    fn is_valid(&self) -> bool {
        canonical_phone(&self.phone_number).is_ok()
    }
}
async fn lookup(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<State>,
    ValidatedJson(body): ValidatedJson<Lookup>,
) -> Result<Json<Entry>, ApiError> {
    let key = PhoneKey::new(&body.phone_number, &state.key).map_err(error)?;
    state.store.lookup(key).await.map(Json).map_err(error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    entry_id: Option<Uuid>,
    phone_number: Option<String>,
    decision: Decision,
    valid_days: Option<u32>,
    version: i64,
    reason: Option<String>,
}
impl ApiDto for ChangeBody {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";
    fn is_valid(&self) -> bool {
        self.entry_id.is_some() != self.phone_number.is_some()
            && self.version >= 0
            && self
                .phone_number
                .as_deref()
                .is_none_or(|p| canonical_phone(p).is_ok())
            && self.decision.accepts_duration(self.valid_days)
            && self
                .reason
                .as_deref()
                .is_none_or(|s| s.encode_utf16().count() <= 500 && !s.chars().any(char::is_control))
    }
}
async fn change(
    RecentAdminIdentity(actor): RecentAdminIdentity,
    Extension(state): Extension<State>,
    ValidatedJson(body): ValidatedJson<ChangeBody>,
) -> Result<StatusCode, ApiError> {
    let target = match (body.entry_id, body.phone_number) {
        (Some(id), None) => Target::Id(id),
        (None, Some(phone)) => Target::Phone(PhoneKey::new(&phone, &state.key).map_err(error)?),
        _ => return Err(error(PolicyError::Invalid)),
    };
    state
        .store
        .change(Change {
            actor: actor.user_id,
            target,
            decision: body.decision,
            days: body.valid_days,
            version: body.version,
            reason: body.reason,
        })
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
fn error(error: PolicyError) -> ApiError {
    match error {
        PolicyError::Forbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            "admin_action_forbidden",
            "This administrator action is not allowed.",
        ),
        PolicyError::Invalid => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_admin_request",
            "The administrator request is invalid.",
        ),
        PolicyError::Conflict => ApiError::new(
            StatusCode::CONFLICT,
            "resource_changed",
            "This resource has changed. Reload it before making a decision.",
        ),
        PolicyError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "resource_not_found",
            "The requested resource was not found.",
        ),
        PolicyError::Unavailable | PolicyError::KeyMismatch => ApiError::internal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutations_have_one_target_bounded_duration_and_never_accept_unknown_fields() {
        let mut body = serde_json::json!({"entry_id":Uuid::new_v4(),"decision":"allow","version":0,"valid_days":30});
        assert!(
            serde_json::from_value::<ChangeBody>(body.clone())
                .unwrap()
                .is_valid()
        );
        body["phone_number"] = serde_json::json!("+33600000000");
        assert!(
            !serde_json::from_value::<ChangeBody>(body.clone())
                .unwrap()
                .is_valid()
        );
        body.as_object_mut().unwrap().remove("phone_number");
        for days in [0, 91] {
            body["valid_days"] = serde_json::json!(days);
            assert!(
                !serde_json::from_value::<ChangeBody>(body.clone())
                    .unwrap()
                    .is_valid()
            );
        }
        body["valid_days"] = serde_json::json!(30);
        body["role"] = serde_json::json!("superadmin");
        assert!(serde_json::from_value::<ChangeBody>(body).is_err());
    }
}
