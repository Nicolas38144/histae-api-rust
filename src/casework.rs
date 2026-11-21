use axum::{
    Json, Router,
    extract::{Extension, Path, Query},
    http::StatusCode,
    middleware::from_fn,
    routing::{get, patch, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    config::LimitPolicy,
    http::{error::ApiError, extract::{ApiDto, ValidatedJson}, lifecycle::ClientIp, rate_limit::RateLimiter, router::HttpState},
    identity::{
        admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity},
        mobile::http::{AuthenticatedMobile, MobileAuthState},
        web::http::no_store,
    },
    infra::{crypto::sha256_hex, postgres::{Database, map_sqlx_error}},
    notifications::{domain::NotificationIntent, enqueue_notification},
    privacy::{export::{DataExportService, http::download}, rights::valid_request_text},
    shared::text::javascript_trim,
};

#[derive(Clone)]
pub struct Casework {
    database: Database,
    limiter: RateLimiter,
    policy: LimitPolicy,
    export: DataExportService,
}

impl Casework {
    pub fn new(database: Database, limiter: RateLimiter, policy: LimitPolicy, export: DataExportService) -> Self {
        Self { database, limiter, policy, export }
    }

    async fn limit(&self, namespace: &'static str, subject: &str) -> Result<(), ApiError> {
        self.limiter.enforce(namespace, subject, &self.policy, "casework_rate_limit_exceeded").await
    }
}

pub fn routes(state: Casework, mobile: MobileAuthState, admin: AdminAuthHttpState) -> Router<HttpState> {
    let mobile = Router::new()
        .route("/api/users/me/reports", get(my_reports))
        .route("/api/users/me/data-subject-requests/{id}/messages", get(my_thread).post(user_message))
        .route("/api/users/me/data-subject-requests/{id}/assisted-export", get(my_export))
        .route("/api/users/me/assisted-exports/{id}", get(download_export))
        .route("/api/users/me/erasure-receipt", post(issue_receipt))
        .route("/api/users/me/support-cases", get(my_support).post(create_support))
        .layer(Extension(mobile));
    let admin = Router::new()
        .route("/api/admin/reports/{id}/decision", patch(review_report))
        .route("/api/admin/data-subject-requests/{id}/messages", get(admin_thread).post(team_message))
        .route("/api/admin/data-subject-requests/{id}/assisted-export", get(admin_export).post(grant_export))
        .route("/api/admin/support-cases", get(admin_support))
        .route("/api/admin/support-cases/{id}", patch(review_support))
        .layer(Extension(admin));
    Router::new()
        .route("/api/public/erasure-status", post(receipt_status))
        .route("/api/public/support-cases", post(create_guest_support))
        .route("/api/public/support-cases/status", post(guest_support_status))
        .merge(mobile).merge(admin)
        .layer(from_fn(no_store))
        .layer(Extension(state))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<Uuid>,
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "case_not_found", "This case is unavailable.")
}

fn conflict() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "case_action_unavailable", "This case no longer allows that action.")
}

fn nonempty(value: &str) -> bool {
    !javascript_trim(value).is_empty() && valid_request_text(Some(value))
}

async fn my_reports(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<Casework>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'reason',reason,'description',description,
           'status',CASE WHEN status='pending' THEN 'pending' ELSE 'closed' END,
           'response',public_response,'created_at',created_at,'closed_at',resolved_at)
         FROM user_report r WHERE reporter_id=$1 AND ($2::uuid IS NULL OR (r.created_at,r.id) <
           (SELECT b.created_at,b.id FROM user_report b WHERE b.id=$2 AND b.reporter_id=$1))
         ORDER BY created_at DESC,id DESC LIMIT 51")
        .bind(identity.account.user_id).bind(page.before)
        .fetch_all(state.database.pool()).await.map_err(map_sqlx_error)?;
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
    let mut tx = state.database.pool().begin().await.map_err(map_sqlx_error)?;
    let reported = sqlx::query_scalar::<_, Uuid>(
        "UPDATE user_report SET status=$2, public_response=$3, resolved_at=COALESCE(resolved_at,clock_timestamp())
         WHERE id=$1 AND status='pending' RETURNING reported_id")
        .bind(id).bind(&body.status).bind(javascript_trim(&body.response))
        .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?.ok_or_else(conflict)?;
    sqlx::query("INSERT INTO data_access_log (accessed_user_id,accessor_id,accessor_role,action,reason)
                 VALUES ($1,$2,$3,'admin_review_report',$4)")
        .bind(reported).bind(identity.user_id).bind(identity.role.as_str())
        .bind(format!("Report {id} moved to {}", body.status))
        .execute(&mut *tx).await.map_err(map_sqlx_error)?;
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn my_thread(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>, Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    thread(&state, id, Some(identity.account.user_id), page.before).await
}

async fn admin_thread(
    AdminIdentity(_): AdminIdentity, Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>, Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    thread(&state, id, None, page.before).await
}

async fn thread(state: &Casework, id: Uuid, owner: Option<Uuid>, before: Option<Uuid>) -> Result<Json<Value>, ApiError> {
    let initial: Option<(Option<String>, DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT details,requested_at,status FROM data_subject_request WHERE id=$1 AND ($2::uuid IS NULL OR user_id=$2)")
        .bind(id).bind(owner).fetch_optional(state.database.pool()).await.map_err(map_sqlx_error)?;
    let (details, requested_at, status) = initial.ok_or_else(not_found)?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',m.id,'author',m.author,'body',m.body,'created_at',m.created_at)
         FROM data_request_message m WHERE request_id=$1
         AND ($2::uuid IS NULL OR (m.created_at,m.id) <
           (SELECT b.created_at,b.id FROM data_request_message b WHERE b.id=$2 AND b.request_id=$1))
         ORDER BY created_at DESC,id DESC LIMIT 51")
        .bind(id).bind(before).fetch_all(state.database.pool()).await.map_err(map_sqlx_error)?;
    let Json(mut result) = paginated("messages", rows);
    result["details"] = json!(details);
    result["requested_at"] = json!(requested_at);
    result["status"] = json!(status);
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    message: String,
}

impl ApiDto for Message { fn is_valid(&self) -> bool { nonempty(&self.message) } }

async fn user_message(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>, ValidatedJson(body): ValidatedJson<Message>,
) -> Result<StatusCode, ApiError> {
    let owner = identity.account.user_id;
    state.limit("rights-message", &owner.to_string()).await?;
    let mut tx = state.database.pool().begin().await.map_err(map_sqlx_error)?;
    let open = sqlx::query_scalar::<_, Uuid>(
        "SELECT r.id FROM data_subject_request r JOIN user_account a ON a.user_id=r.user_id
         WHERE r.id=$1 AND r.user_id=$2 AND a.deleted_at IS NULL AND NOT a.is_banned
           AND r.status IN ('pending','in_progress') AND r.type NOT IN ('restriction','objection') FOR UPDATE OF r")
        .bind(id).bind(owner).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
    if open.is_none() { return Err(not_found()); }
    let stored = sqlx::query("INSERT INTO data_request_message(request_id,author,body)
        SELECT $1,'user',$2 WHERE (SELECT count(*) FROM data_request_message WHERE request_id=$1 AND author='user') < 100")
        .bind(id).bind(javascript_trim(&body.message)).execute(&mut *tx).await.map_err(map_sqlx_error)?;
    if stored.rows_affected() == 0 { return Err(conflict()); }
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn team_message(
    RecentAdminIdentity(identity): RecentAdminIdentity, Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>, ValidatedJson(body): ValidatedJson<Message>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.database.pool().begin().await.map_err(map_sqlx_error)?;
    // Keep the same account-then-request lock order as the status transition.
    let user: Uuid = sqlx::query_scalar(
        "SELECT a.user_id FROM data_subject_request r JOIN user_account a ON a.user_id=r.user_id
         WHERE r.id=$1 AND a.deleted_at IS NULL AND NOT a.is_banned FOR SHARE OF a")
        .bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?.ok_or_else(conflict)?;
    let (status, previous): (String, Option<String>) = sqlx::query_as(
        "SELECT status,response FROM data_subject_request
         WHERE id=$1 AND user_id=$2 AND type NOT IN ('restriction','objection') FOR UPDATE")
        .bind(id).bind(user).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?.ok_or_else(conflict)?;
    let message = javascript_trim(&body.message);
    if previous.as_deref() == Some(message) {
        return Ok(StatusCode::NO_CONTENT);
    }
    sqlx::query("UPDATE data_subject_request SET response=$2 WHERE id=$1")
        .bind(id).bind(message).execute(&mut *tx).await.map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO data_access_log(accessed_user_id,accessor_id,accessor_role,action,reason)
                 VALUES ($1,$2,$3,'admin_review_dsr','Sent a case message')")
        .bind(user).bind(identity.user_id).bind(identity.role.as_str())
        .execute(&mut *tx).await.map_err(map_sqlx_error)?;
    let source = format!("{id}:{}", Uuid::new_v4());
    enqueue_notification(&mut tx, user, &source, &NotificationIntent::DataRequestReplied {
        request_id: id, response_status: status,
    }).await?;
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct ExportGrant {
    id: Uuid,
    expires_at: DateTime<Utc>,
}

impl From<(Uuid, DateTime<Utc>)> for ExportGrant {
    fn from((id, expires_at): (Uuid, DateTime<Utc>)) -> Self {
        Self { id, expires_at }
    }
}

async fn my_export(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>, Path(id): Path<Uuid>,
) -> Result<Json<Option<ExportGrant>>, ApiError> {
    export_grant(&state, id, Some(identity.account.user_id)).await
}

async fn download_export(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>, Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    let user = identity.account.user_id;
    state.limit("assisted-export-download", &user.to_string()).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assisted_data_export e JOIN data_subject_request r ON r.id=e.request_id
         WHERE e.id=$1 AND r.user_id=$2 AND e.expires_at>clock_timestamp() AND r.status<>'rejected')")
        .bind(id).bind(user).fetch_one(state.database.pool()).await.map_err(map_sqlx_error)?;
    if !exists { return Err(not_found()); }
    download(state.export.prepare_assisted(user).await)
}

async fn admin_export(
    AdminIdentity(_): AdminIdentity, Extension(state): Extension<Casework>, Path(id): Path<Uuid>,
) -> Result<Json<Option<ExportGrant>>, ApiError> {
    export_grant(&state, id, None).await
}

async fn export_grant(state: &Casework, request_id: Uuid, owner: Option<Uuid>) -> Result<Json<Option<ExportGrant>>, ApiError> {
    sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
        "SELECT e.id,e.expires_at FROM assisted_data_export e JOIN data_subject_request r ON r.id=e.request_id
         WHERE e.request_id=$1 AND ($2::uuid IS NULL OR r.user_id=$2) AND e.expires_at>clock_timestamp()
           AND r.status<>'rejected'")
        .bind(request_id).bind(owner).fetch_optional(state.database.pool()).await
        .map(|row| Json(row.map(ExportGrant::from))).map_err(map_sqlx_error).map_err(Into::into)
}

async fn grant_export(
    RecentAdminIdentity(identity): RecentAdminIdentity, Extension(state): Extension<Casework>, Path(id): Path<Uuid>,
) -> Result<Json<ExportGrant>, ApiError> {
    let mut tx = state.database.pool().begin().await.map_err(map_sqlx_error)?;
    let user = sqlx::query_scalar::<_, Uuid>(
        "SELECT r.user_id FROM data_subject_request r JOIN user_account a ON a.user_id=r.user_id
         WHERE r.id=$1 AND r.type IN ('access','portability') AND r.status IN ('in_progress','completed')
           AND a.deleted_at IS NULL AND NOT a.is_banned FOR UPDATE OF r")
        .bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?.ok_or_else(conflict)?;
    let grant: (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO assisted_data_export(request_id,expires_at) VALUES ($1,clock_timestamp()+interval '24 hours')
         ON CONFLICT (request_id) DO UPDATE SET
           id=CASE WHEN assisted_data_export.expires_at<=clock_timestamp() THEN gen_random_uuid() ELSE assisted_data_export.id END,
           expires_at=CASE WHEN assisted_data_export.expires_at<=clock_timestamp() THEN EXCLUDED.expires_at ELSE assisted_data_export.expires_at END
         RETURNING id,expires_at")
        .bind(id).fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO data_access_log(accessed_user_id,accessor_id,accessor_role,action,reason)
                 VALUES ($1,$2,$3,'admin_review_dsr','Granted an assisted export')")
        .bind(user).bind(identity.user_id).bind(identity.role.as_str())
        .execute(&mut *tx).await.map_err(map_sqlx_error)?;
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(Json(grant.into()))
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
        self.code.len() == 43 && URL_SAFE_NO_PAD.decode(&self.code).is_ok_and(|value| value.len() == 32)
    }
}

async fn issue_receipt(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>,
) -> Result<Json<Value>, ApiError> {
    let user = identity.account.user_id;
    state.limit("erasure-receipt", &user.to_string()).await?;
    let code = new_token()?;
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO erasure_receipt(user_id,token_hash,expires_at)
         SELECT user_id,$2,clock_timestamp()+interval '90 days' FROM user_account
         WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned
         ON CONFLICT (user_id) DO UPDATE SET token_hash=EXCLUDED.token_hash,expires_at=EXCLUDED.expires_at
         RETURNING expires_at")
        .bind(user).bind(sha256_hex(code.as_bytes()))
        .fetch_optional(state.database.pool()).await.map_err(map_sqlx_error)?.ok_or_else(not_found)?;
    Ok(Json(json!({ "code": code, "expires_at": expires_at })))
}

async fn receipt_status(
    Extension(state): Extension<Casework>, Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<Secret>,
) -> Result<Json<Value>, ApiError> {
    state.limit("erasure-status", &ip.to_string()).await?;
    let row: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT CASE WHEN e.completed_at IS NOT NULL THEN 'completed'
                     WHEN e.request_id IS NOT NULL THEN 'in_progress'
                     ELSE 'not_started' END, e.completed_at
         FROM erasure_receipt r LEFT JOIN account_erasure e ON e.user_id=r.user_id
         WHERE r.token_hash=$1 AND r.expires_at>clock_timestamp()")
        .bind(sha256_hex(body.code.as_bytes())).fetch_optional(state.database.pool()).await.map_err(map_sqlx_error)?;
    let (status, completed_at) = row.ok_or_else(not_found)?;
    Ok(Json(json!({ "status": status, "completed_at": completed_at })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportDraft {
    category: String,
    message: String,
}

impl ApiDto for SupportDraft {
    fn is_valid(&self) -> bool {
        matches!(self.category.as_str(), "login" | "billing" | "appeal" | "privacy" | "other") && nonempty(&self.message)
    }
}

async fn create_support(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>,
    ValidatedJson(body): ValidatedJson<SupportDraft>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = identity.account.user_id;
    state.limit("support-create-user", &user.to_string()).await?;
    let id = insert_support(&state, Some(user), None, body).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn create_guest_support(
    Extension(state): Extension<Casework>, Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<SupportDraft>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.limit("support-create-guest", &ip.to_string()).await?;
    let code = new_token()?;
    let id = insert_support(&state, None, Some(sha256_hex(code.as_bytes())), body).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "code": code }))))
}

async fn insert_support(state: &Casework, user: Option<Uuid>, hash: Option<String>, body: SupportDraft) -> Result<Uuid, ApiError> {
    sqlx::query_scalar(
        "INSERT INTO support_case(user_id,token_hash,category,body) SELECT $1,$2,$3,$4
         WHERE $1::uuid IS NULL OR EXISTS(SELECT 1 FROM user_account
           WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE)
         RETURNING id")
        .bind(user).bind(hash).bind(body.category).bind(javascript_trim(&body.message))
        .fetch_optional(state.database.pool()).await.map_err(map_sqlx_error)?.ok_or_else(not_found)
}

async fn guest_support_status(
    Extension(state): Extension<Casework>, Extension(ClientIp(ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<Secret>,
) -> Result<Json<Value>, ApiError> {
    state.limit("support-status", &ip.to_string()).await?;
    sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'category',category,'body',body,'status',status,
                  'response',response,'created_at',created_at,'updated_at',updated_at,'expires_at',expires_at)
         FROM support_case
         WHERE token_hash=$1 AND user_id IS NULL AND expires_at>clock_timestamp()")
        .bind(sha256_hex(body.code.as_bytes())).fetch_optional(state.database.pool()).await
        .map_err(map_sqlx_error)?.map(Json).ok_or_else(not_found)
}

async fn my_support(
    AuthenticatedMobile(identity): AuthenticatedMobile, Extension(state): Extension<Casework>, Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    support_list(&state, Some(identity.account.user_id), page.before).await
}

async fn admin_support(
    AdminIdentity(_): AdminIdentity, Extension(state): Extension<Casework>, Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    support_list(&state, None, page.before).await
}

async fn support_list(state: &Casework, owner: Option<Uuid>, before: Option<Uuid>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',s.id,'category',s.category,'body',s.body,'status',s.status,
                  'response',s.response,'created_at',s.created_at,'updated_at',s.updated_at,'expires_at',s.expires_at)
             || CASE WHEN $1::uuid IS NULL THEN jsonb_build_object('user_id',s.user_id,'handled_by',s.handled_by)
                     ELSE '{}'::jsonb END
         FROM support_case s WHERE ($1::uuid IS NULL OR user_id=$1) AND expires_at>clock_timestamp()
         AND ($2::uuid IS NULL OR (s.created_at,s.id) <
           (SELECT b.created_at,b.id FROM support_case b WHERE b.id=$2 AND ($1::uuid IS NULL OR b.user_id=$1)))
         ORDER BY created_at DESC,id DESC LIMIT 51")
        .bind(owner).bind(before).fetch_all(state.database.pool()).await.map_err(map_sqlx_error)?;
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
    RecentAdminIdentity(identity): RecentAdminIdentity, Extension(state): Extension<Casework>,
    Path(id): Path<Uuid>, ValidatedJson(body): ValidatedJson<SupportReview>,
) -> Result<StatusCode, ApiError> {
    let rows = sqlx::query(
        "UPDATE support_case SET status=$2,response=$3,handled_by=$4,updated_at=clock_timestamp()
         WHERE id=$1 AND status<>'closed' AND expires_at>clock_timestamp()")
        .bind(id).bind(body.status).bind(javascript_trim(&body.response)).bind(identity.user_id)
        .execute(state.database.pool()).await.map_err(map_sqlx_error)?.rows_affected();
    if rows == 0 { return Err(conflict()); }
    Ok(StatusCode::NO_CONTENT)
}

fn paginated(key: &str, mut rows: Vec<Value>) -> Json<Value> {
    let has_more = rows.len() > 50;
    rows.truncate(50);
    let next = if has_more { rows.last().and_then(|row| row.get("id")).cloned() } else { None };
    let mut value = json!({ "next_cursor": next });
    value[key] = json!(rows);
    Json(value)
}
