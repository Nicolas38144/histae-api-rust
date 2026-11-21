mod pg;

use pg::{CaseworkRepository, ExportGrant, MessageOutcome, ReceiptStatus};

use axum::{
    Json, Router,
    extract::{Extension, Path, Query},
    http::StatusCode,
    middleware::from_fn,
    routing::{get, patch, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    config::LimitPolicy,
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson},
        lifecycle::ClientIp,
        rate_limit::RateLimiter,
        router::HttpState,
    },
    identity::{
        admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity},
        mobile::http::{AuthenticatedMobile, MobileAuthState},
        web::http::no_store,
    },
    infra::{crypto::sha256_hex, postgres::Database},
    privacy::{
        export::{DataExportService, http::download},
        rights::valid_request_text,
    },
    shared::text::javascript_trim,
};

#[derive(Clone)]
pub struct Casework {
    repository: CaseworkRepository,
    limiter: RateLimiter,
    policy: LimitPolicy,
    export: DataExportService,
}

impl Casework {
    pub fn new(
        database: Database,
        limiter: RateLimiter,
        policy: LimitPolicy,
        export: DataExportService,
    ) -> Self {
        Self {
            repository: CaseworkRepository(database),
            limiter,
            policy,
            export,
        }
    }

    async fn limit(&self, namespace: &'static str, subject: &str) -> Result<(), ApiError> {
        self.limiter
            .enforce(
                namespace,
                subject,
                &self.policy,
                "casework_rate_limit_exceeded",
            )
            .await
    }
}

pub fn routes(
    state: Casework,
    mobile: MobileAuthState,
    admin: AdminAuthHttpState,
) -> Router<HttpState> {
    let mobile = Router::new()
        .route("/api/users/me/reports", get(my_reports))
        .route(
            "/api/users/me/data-subject-requests/{id}/messages",
            get(my_thread).post(user_message),
        )
        .route(
            "/api/users/me/data-subject-requests/{id}/assisted-export",
            get(my_export),
        )
        .route("/api/users/me/assisted-exports/{id}", get(download_export))
        .route("/api/users/me/erasure-receipt", post(issue_receipt))
        .route(
            "/api/users/me/support-cases",
            get(my_support).post(create_support),
        )
        .layer(Extension(mobile));
    let admin = Router::new()
        .route("/api/admin/reports/{id}/decision", patch(review_report))
        .route(
            "/api/admin/data-subject-requests/{id}/messages",
            get(admin_thread).post(team_message),
        )
        .route(
            "/api/admin/data-subject-requests/{id}/assisted-export",
            get(admin_export).post(grant_export),
        )
        .route("/api/admin/support-cases", get(admin_support))
        .route("/api/admin/support-cases/{id}", patch(review_support))
        .layer(Extension(admin));
    Router::new()
        .route("/api/public/erasure-status", post(receipt_status))
        .route("/api/public/support-cases", post(create_guest_support))
        .route(
            "/api/public/support-cases/status",
            post(guest_support_status),
        )
        .merge(mobile)
        .merge(admin)
        .layer(from_fn(no_store))
        .layer(Extension(state))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<Uuid>,
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "case_not_found",
        "This case is unavailable.",
    )
}

fn conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "case_action_unavailable",
        "This case no longer allows that action.",
    )
}

fn action(applied: bool) -> Result<StatusCode, ApiError> {
    if applied {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(conflict())
    }
}

fn nonempty(value: &str) -> bool {
    !javascript_trim(value).is_empty() && valid_request_text(Some(value))
}

async fn my_reports(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let rows = state
        .repository
        .reports(identity.account.user_id, page.before)
        .await?;
    Ok(paginated("reports", rows))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    status: String,
    response: String,
}

impl ApiDto for Decision {
    fn is_valid(&self) -> bool {
        matches!(self.status.as_str(), "reviewed" | "dismissed") && nonempty(&self.response)
    }
}

async fn review_report(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    ValidatedJson(body): ValidatedJson<Decision>,
) -> Result<StatusCode, ApiError> {
    action(
        state
            .repository
            .review_report(
                id,
                &body.status,
                javascript_trim(&body.response),
                identity.user_id,
                identity.role,
            )
            .await?,
    )
}

async fn my_thread(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    thread(&state, id, Some(identity.account.user_id), page.before).await
}

async fn admin_thread(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    thread(&state, id, None, page.before).await
}

async fn thread(
    state: &Casework,
    id: Uuid,
    owner: Option<Uuid>,
    before: Option<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let thread = state
        .repository
        .thread(id, owner, before)
        .await?
        .ok_or_else(not_found)?;
    let Json(mut result) = paginated("messages", thread.messages);
    result["details"] = json!(thread.details);
    result["requested_at"] = json!(thread.requested_at);
    result["status"] = json!(thread.status);
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    message: String,
}

impl ApiDto for Message {
    fn is_valid(&self) -> bool {
        nonempty(&self.message)
    }
}

async fn user_message(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    ValidatedJson(body): ValidatedJson<Message>,
) -> Result<StatusCode, ApiError> {
    let owner = identity.account.user_id;
    state.limit("rights-message", &owner.to_string()).await?;
    match state
        .repository
        .add_user_message(id, owner, javascript_trim(&body.message))
        .await?
    {
        MessageOutcome::Unavailable => Err(not_found()),
        MessageOutcome::Full => Err(conflict()),
        MessageOutcome::Stored => Ok(StatusCode::NO_CONTENT),
    }
}

async fn team_message(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    ValidatedJson(body): ValidatedJson<Message>,
) -> Result<StatusCode, ApiError> {
    action(
        state
            .repository
            .add_team_message(
                id,
                javascript_trim(&body.message),
                identity.user_id,
                identity.role,
            )
            .await?,
    )
}

async fn my_export(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
) -> Result<Json<Option<ExportGrant>>, ApiError> {
    export_grant(&state, id, Some(identity.account.user_id)).await
}

async fn download_export(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    let user = identity.account.user_id;
    state
        .limit("assisted-export-download", &user.to_string())
        .await?;
    if !state.repository.can_download(id, user).await? {
        return Err(not_found());
    }
    download(state.export.prepare_assisted(user).await)
}

async fn admin_export(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
) -> Result<Json<Option<ExportGrant>>, ApiError> {
    export_grant(&state, id, None).await
}

async fn export_grant(
    state: &Casework,
    request_id: Uuid,
    owner: Option<Uuid>,
) -> Result<Json<Option<ExportGrant>>, ApiError> {
    Ok(Json(
        state.repository.export_grant(request_id, owner).await?,
    ))
}

async fn grant_export(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
) -> Result<Json<ExportGrant>, ApiError> {
    let grant = state
        .repository
        .grant_export(id, identity.user_id, identity.role)
        .await?
        .ok_or_else(conflict)?;
    Ok(Json(grant))
}

fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::internal())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Secret {
    code: String,
}

impl ApiDto for Secret {
    fn is_valid(&self) -> bool {
        self.code.len() == 43
            && URL_SAFE_NO_PAD
                .decode(&self.code)
                .is_ok_and(|value| value.len() == 32)
    }
}

async fn issue_receipt(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
) -> Result<Json<Value>, ApiError> {
    let user = identity.account.user_id;
    state.limit("erasure-receipt", &user.to_string()).await?;
    let code = new_token()?;
    let expires_at = state
        .repository
        .issue_receipt(user, &sha256_hex(code.as_bytes()))
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "code": code, "expires_at": expires_at })))
}

async fn receipt_status(
    Extension(state): Extension<Casework>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<Secret>,
) -> Result<Json<ReceiptStatus>, ApiError> {
    state.limit("erasure-status", &ip.to_string()).await?;
    let status = state
        .repository
        .receipt_status(&sha256_hex(body.code.as_bytes()))
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(status))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportDraft {
    category: String,
    message: String,
}

impl ApiDto for SupportDraft {
    fn is_valid(&self) -> bool {
        matches!(
            self.category.as_str(),
            "login" | "billing" | "appeal" | "privacy" | "other"
        ) && nonempty(&self.message)
    }
}

async fn create_support(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    ValidatedJson(body): ValidatedJson<SupportDraft>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = identity.account.user_id;
    state
        .limit("support-create-user", &user.to_string())
        .await?;
    let id = state
        .repository
        .create_support(
            Some(user),
            None,
            &body.category,
            javascript_trim(&body.message),
        )
        .await?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn create_guest_support(
    Extension(state): Extension<Casework>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<SupportDraft>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.limit("support-create-guest", &ip.to_string()).await?;
    let code = new_token()?;
    let hash = sha256_hex(code.as_bytes());
    let id = state
        .repository
        .create_support(
            None,
            Some(&hash),
            &body.category,
            javascript_trim(&body.message),
        )
        .await?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "code": code }))))
}

async fn guest_support_status(
    Extension(state): Extension<Casework>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<Secret>,
) -> Result<Json<Value>, ApiError> {
    state.limit("support-status", &ip.to_string()).await?;
    state
        .repository
        .guest_support(&sha256_hex(body.code.as_bytes()))
        .await?
        .map(Json)
        .ok_or_else(not_found)
}

async fn my_support(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    support_list(&state, Some(identity.account.user_id), page.before).await
}

async fn admin_support(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<Casework>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    support_list(&state, None, page.before).await
}

async fn support_list(
    state: &Casework,
    owner: Option<Uuid>,
    before: Option<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows = state.repository.support_cases(owner, before).await?;
    Ok(paginated("cases", rows))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportReview {
    status: String,
    response: String,
}

impl ApiDto for SupportReview {
    fn is_valid(&self) -> bool {
        matches!(self.status.as_str(), "in_progress" | "closed") && nonempty(&self.response)
    }
}

async fn review_support(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>,
    ValidatedJson(body): ValidatedJson<SupportReview>,
) -> Result<StatusCode, ApiError> {
    action(
        state
            .repository
            .review_support(
                id,
                &body.status,
                javascript_trim(&body.response),
                identity.user_id,
            )
            .await?,
    )
}

fn paginated(key: &str, mut rows: Vec<Value>) -> Json<Value> {
    let has_more = rows.len() > 50;
    rows.truncate(50);
    let next = if has_more {
        rows.last().and_then(|row| row.get("id")).cloned()
    } else {
        None
    };
    let mut value = json!({ "next_cursor": next });
    value[key] = json!(rows);
    Json(value)
}
