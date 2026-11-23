use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::identity::admin::service::AuthenticatedAdmin;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::notifications::{domain::NotificationIntent, enqueue_notification};

#[derive(Clone)]
pub(super) struct CaseworkRepository(pub(super) Database);

#[derive(Clone, Copy)]
pub(super) enum ThreadAccess<'a> {
    Owner(Uuid),
    Admin(&'a AuthenticatedAdmin, &'a str),
}

#[derive(Serialize)]
pub(super) struct ExportGrant {
    id: Uuid,
    expires_at: DateTime<Utc>,
}

impl From<(Uuid, DateTime<Utc>)> for ExportGrant {
    fn from((id, expires_at): (Uuid, DateTime<Utc>)) -> Self {
        Self { id, expires_at }
    }
}

impl CaseworkRepository {
    pub(super) async fn reports(
        &self,
        owner: Uuid,
        before: Option<Uuid>,
    ) -> Result<Vec<Value>, DatabaseError> {
        sqlx::query_scalar(
            "SELECT jsonb_build_object('id',id,'reason',reason,'description',description,
           'status',CASE WHEN status='pending' THEN 'pending' ELSE 'closed' END,
           'response',public_response,'created_at',created_at,'closed_at',resolved_at)
         FROM user_report r WHERE reporter_id=$1 AND ($2::uuid IS NULL OR (r.created_at,r.id) <
           (SELECT b.created_at,b.id FROM user_report b WHERE b.id=$2 AND b.reporter_id=$1))
         ORDER BY created_at DESC,id DESC LIMIT 51",
        )
        .bind(owner)
        .bind(before)
        .fetch_all(self.0.pool())
        .await
        .map_err(map_sqlx_error)
    }

    pub(super) async fn review_report(
        &self,
        id: Uuid,
        status: &str,
        response: &str,
        admin: &AuthenticatedAdmin,
    ) -> Result<bool, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        let reported = sqlx::query_scalar::<_, Uuid>(
        "UPDATE user_report SET status=$2, public_response=$3, resolved_at=COALESCE(resolved_at,clock_timestamp())
         WHERE id=$1 AND status='pending' RETURNING reported_id")
        .bind(id).bind(status).bind(response)
        .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some(reported) = reported else {
            return Ok(false);
        };
        audit(
            &mut tx,
            reported,
            admin,
            "admin_review_report",
            &format!("Report {id} moved to {status}"),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(true)
    }

    pub(super) async fn thread(
        &self,
        id: Uuid,
        access: ThreadAccess<'_>,
        before: Option<Uuid>,
    ) -> Result<Option<Thread>, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        let owner = match access {
            ThreadAccess::Owner(user) => Some(user),
            ThreadAccess::Admin(admin, _) => {
                lock_admin(&mut tx, admin).await?;
                None
            }
        };
        let initial: Option<(Uuid, Option<String>, DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT user_id,details,requested_at,status FROM data_subject_request WHERE id=$1 AND ($2::uuid IS NULL OR user_id=$2) FOR SHARE")
        .bind(id).bind(owner).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some((user, details, requested_at, status)) = initial else {
            return Ok(None);
        };
        let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',m.id,'author',m.author,'body',m.body,'created_at',m.created_at)
         FROM data_request_message m WHERE request_id=$1
         AND ($2::uuid IS NULL OR (m.created_at,m.id) <
           (SELECT b.created_at,b.id FROM data_request_message b WHERE b.id=$2 AND b.request_id=$1))
         ORDER BY created_at DESC,id DESC LIMIT 51")
        .bind(id).bind(before).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        if let ThreadAccess::Admin(admin, reason) = access {
            audit(&mut tx, user, admin, "view_dsr_thread", reason).await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(Thread {
            details,
            requested_at,
            status,
            messages: rows,
        }))
    }

    pub(super) async fn add_user_message(
        &self,
        id: Uuid,
        owner: Uuid,
        message: &str,
    ) -> Result<MessageOutcome, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        if !lock_live_account(&mut tx, owner).await? {
            return Ok(MessageOutcome::Unavailable);
        }
        let open = sqlx::query_scalar::<_, Uuid>(
        "SELECT r.id FROM data_subject_request r
         WHERE r.id=$1 AND r.user_id=$2
           AND r.status IN ('pending','in_progress') AND r.type NOT IN ('restriction','objection') FOR UPDATE OF r")
        .bind(id).bind(owner).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        if open.is_none() {
            return Ok(MessageOutcome::Unavailable);
        }
        let stored = sqlx::query("INSERT INTO data_request_message(request_id,author,body)
        SELECT $1,'user',$2 WHERE (SELECT count(*) FROM data_request_message WHERE request_id=$1 AND author='user') < 100")
        .bind(id).bind(message).execute(&mut *tx).await.map_err(map_sqlx_error)?;
        if stored.rows_affected() == 0 {
            return Ok(MessageOutcome::Full);
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(MessageOutcome::Stored)
    }

    pub(super) async fn add_team_message(
        &self,
        id: Uuid,
        message: &str,
        admin: &AuthenticatedAdmin,
    ) -> Result<bool, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        // Match the account-then-request lock order used by status transitions.
        let user: Option<Uuid> = sqlx::query_scalar(
        "SELECT a.user_id FROM data_subject_request r JOIN user_account a ON a.user_id=r.user_id
         WHERE r.id=$1 AND a.deleted_at IS NULL AND NOT a.is_banned FOR SHARE OF a",
    ).bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some(user) = user else {
            return Ok(false);
        };
        let request: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM data_subject_request
         WHERE id=$1 AND user_id=$2 AND type NOT IN ('restriction','objection') FOR UPDATE",
        )
        .bind(id)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if request.is_none() {
            return Ok(false);
        }
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT body FROM data_request_message WHERE request_id=$1 AND author='team'
         ORDER BY created_at DESC,id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if previous.as_deref() == Some(message) {
            return Ok(true);
        }
        let message_id: Uuid = sqlx::query_scalar(
            "INSERT INTO data_request_message(request_id,author,body)
         VALUES ($1,'team',$2) RETURNING id",
        )
        .bind(id)
        .bind(message)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        audit(
            &mut tx,
            user,
            admin,
            "admin_review_dsr",
            "Sent a case message",
        )
        .await?;
        enqueue_notification(
            &mut tx,
            user,
            &message_id.to_string(),
            &NotificationIntent::DataRequestMessage {
                request_id: id,
                message_id,
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(true)
    }

    pub(super) async fn can_download(&self, id: Uuid, user: Uuid) -> Result<bool, DatabaseError> {
        sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assisted_data_export e JOIN data_subject_request r ON r.id=e.request_id
         WHERE e.id=$1 AND r.user_id=$2 AND e.expires_at>clock_timestamp() AND r.status<>'rejected')")
        .bind(id).bind(user).fetch_one(self.0.pool()).await.map_err(map_sqlx_error)
    }

    pub(super) async fn export_grant(
        &self,
        request_id: Uuid,
        owner: Option<Uuid>,
    ) -> Result<Option<ExportGrant>, DatabaseError> {
        sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
        "SELECT e.id,e.expires_at FROM assisted_data_export e JOIN data_subject_request r ON r.id=e.request_id
         WHERE e.request_id=$1 AND ($2::uuid IS NULL OR r.user_id=$2) AND e.expires_at>clock_timestamp()
           AND r.status<>'rejected'")
        .bind(request_id).bind(owner).fetch_optional(self.0.pool()).await
        .map(|row| row.map(ExportGrant::from)).map_err(map_sqlx_error)
    }

    pub(super) async fn grant_export(
        &self,
        id: Uuid,
        admin: &AuthenticatedAdmin,
    ) -> Result<Option<ExportGrant>, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        let user: Option<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT r.user_id FROM data_subject_request r JOIN user_account a ON a.user_id=r.user_id
         WHERE r.id=$1 AND r.type IN ('access','portability') AND r.status IN ('in_progress','completed')
           AND a.deleted_at IS NULL AND NOT a.is_banned FOR SHARE OF a")
        .bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some(user) = user else {
            return Ok(None);
        };
        let request: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM data_subject_request WHERE id=$1 AND user_id=$2
           AND type IN ('access','portability') AND status IN ('in_progress','completed') FOR UPDATE",
    ).bind(id).bind(user).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        if request.is_none() {
            return Ok(None);
        }
        let grant: (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO assisted_data_export(request_id,expires_at) VALUES ($1,clock_timestamp()+interval '24 hours')
         ON CONFLICT (request_id) DO UPDATE SET
           id=CASE WHEN assisted_data_export.expires_at<=clock_timestamp() THEN gen_random_uuid() ELSE assisted_data_export.id END,
           expires_at=CASE WHEN assisted_data_export.expires_at<=clock_timestamp() THEN EXCLUDED.expires_at ELSE assisted_data_export.expires_at END
         RETURNING id,expires_at")
        .bind(id).fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
        audit(
            &mut tx,
            user,
            admin,
            "admin_review_dsr",
            "Granted an assisted export",
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(grant.into()))
    }

    pub(super) async fn admin_export(
        &self,
        request_id: Uuid,
        admin: &AuthenticatedAdmin,
        reason: &str,
    ) -> Result<Option<ExportGrant>, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        let row: Option<(Uuid, Uuid, DateTime<Utc>)> = sqlx::query_as(
            "SELECT r.user_id,e.id,e.expires_at FROM assisted_data_export e
             JOIN data_subject_request r ON r.id=e.request_id
             WHERE r.id=$1 AND e.expires_at>clock_timestamp() FOR SHARE OF r, e",
        )
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some((user, id, expires_at)) = row else {
            return Ok(None);
        };
        audit(&mut tx, user, admin, "view_dsr_export", reason).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some((id, expires_at).into()))
    }

    pub(super) async fn issue_receipt(
        &self,
        user: Uuid,
        hash: &str,
    ) -> Result<Option<DateTime<Utc>>, DatabaseError> {
        sqlx::query_scalar(
        "INSERT INTO erasure_receipt(user_id,token_hash,expires_at)
         SELECT user_id,$2,clock_timestamp()+interval '90 days' FROM user_account
         WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE
         ON CONFLICT (user_id) DO UPDATE SET token_hash=EXCLUDED.token_hash,expires_at=EXCLUDED.expires_at
         RETURNING expires_at")
        .bind(user).bind(hash)
        .fetch_optional(self.0.pool()).await.map_err(map_sqlx_error)
    }

    pub(super) async fn receipt_status(
        &self,
        hash: &str,
    ) -> Result<Option<ReceiptStatus>, DatabaseError> {
        sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
            "SELECT CASE WHEN e.completed_at IS NOT NULL THEN 'completed'
                     WHEN e.request_id IS NOT NULL THEN 'in_progress'
                     ELSE 'not_started' END, e.completed_at
         FROM erasure_receipt r LEFT JOIN account_erasure e ON e.user_id=r.user_id
         WHERE r.token_hash=$1 AND r.expires_at>clock_timestamp()",
        )
        .bind(hash)
        .fetch_optional(self.0.pool())
        .await
        .map(|row| {
            row.map(|(status, completed_at)| ReceiptStatus {
                status,
                completed_at,
            })
        })
        .map_err(map_sqlx_error)
    }

    pub(super) async fn create_support(
        &self,
        user: Option<Uuid>,
        hash: Option<&str>,
        category: &str,
        message: &str,
    ) -> Result<Option<Uuid>, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        if user.is_none() {
            // Serialize admission across replicas without queuing work in the pool.
            let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(58461732, 1)")
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            if !locked {
                return Ok(None);
            }
        }
        let id = sqlx::query_scalar(
            "INSERT INTO support_case(user_id,token_hash,category,body) SELECT $1,$2,$3,$4
         WHERE CASE WHEN $1::uuid IS NULL THEN NOT EXISTS (
           SELECT 1 FROM support_case WHERE user_id IS NULL AND status <> 'closed'
             AND expires_at > statement_timestamp() LIMIT 1 OFFSET 999
         ) ELSE EXISTS (SELECT 1 FROM user_account
           WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE) END
         RETURNING id",
        )
        .bind(user)
        .bind(hash)
        .bind(category)
        .bind(message)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(id)
    }

    pub(super) async fn guest_support(&self, hash: &str) -> Result<Option<Value>, DatabaseError> {
        sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'category',category,'body',body,'status',status,
                  'response',response,'created_at',created_at,'updated_at',updated_at,'expires_at',expires_at)
         FROM support_case
         WHERE token_hash=$1 AND user_id IS NULL AND expires_at>clock_timestamp()")
        .bind(hash).fetch_optional(self.0.pool()).await
        .map_err(map_sqlx_error)
    }

    pub(super) async fn support_cases(
        &self,
        owner: Option<Uuid>,
        before: Option<Uuid>,
    ) -> Result<Vec<Value>, DatabaseError> {
        sqlx::query_scalar(
        "SELECT jsonb_build_object('id',s.id,'category',s.category,'status',s.status,
                  'created_at',s.created_at,'updated_at',s.updated_at,'expires_at',s.expires_at)
             || CASE WHEN $1::uuid IS NULL THEN jsonb_build_object('user_id',s.user_id,'handled_by',s.handled_by)
                     ELSE jsonb_build_object('body',s.body,'response',s.response) END
         FROM support_case s WHERE ($1::uuid IS NULL OR user_id=$1) AND expires_at>clock_timestamp()
         AND ($2::uuid IS NULL OR (s.created_at,s.id) <
           (SELECT b.created_at,b.id FROM support_case b WHERE b.id=$2 AND ($1::uuid IS NULL OR b.user_id=$1)))
         ORDER BY created_at DESC,id DESC LIMIT 51")
        .bind(owner).bind(before).fetch_all(self.0.pool()).await.map_err(map_sqlx_error)
    }

    pub(super) async fn support_detail(
        &self,
        id: Uuid,
        admin: &AuthenticatedAdmin,
        reason: &str,
    ) -> Result<Option<Value>, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        let row: Option<(Option<Uuid>, Value)> = sqlx::query_as(
            "SELECT user_id,jsonb_build_object('id',id,'category',category,'body',body,'status',status,
               'response',response,'user_id',user_id,'handled_by',handled_by,
               'created_at',created_at,'updated_at',updated_at,'expires_at',expires_at)
             FROM support_case WHERE id=$1 AND expires_at>clock_timestamp() FOR SHARE",
        ).bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some((user, detail)) = row else {
            return Ok(None);
        };
        // Guest cases have no account identifier. Use the case ID as the audit subject.
        audit(
            &mut tx,
            user.unwrap_or(id),
            admin,
            "view_support_case",
            reason,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(detail))
    }

    pub(super) async fn review_support(
        &self,
        id: Uuid,
        status: &str,
        response: &str,
        admin: &AuthenticatedAdmin,
    ) -> Result<bool, DatabaseError> {
        let mut tx = self.0.pool().begin().await.map_err(map_sqlx_error)?;
        lock_admin(&mut tx, admin).await?;
        let subject: Option<Uuid> = sqlx::query_scalar(
        "UPDATE support_case SET status=$2,response=$3,handled_by=$4,updated_at=clock_timestamp()
         WHERE id=$1 AND status<>'closed' AND expires_at>clock_timestamp()
         RETURNING COALESCE(user_id,id)")
        .bind(id).bind(status).bind(response).bind(admin.user_id)
        .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        let Some(subject) = subject else {
            return Ok(false);
        };
        audit(
            &mut tx,
            subject,
            admin,
            "admin_review_support",
            &format!("Support case {id} moved to {status}"),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(true)
    }
}

pub(super) struct Thread {
    pub(super) details: Option<String>,
    pub(super) requested_at: DateTime<Utc>,
    pub(super) status: String,
    pub(super) messages: Vec<Value>,
}

pub(super) enum MessageOutcome {
    Unavailable,
    Full,
    Stored,
}

#[derive(Serialize)]
pub(super) struct ReceiptStatus {
    status: String,
    completed_at: Option<DateTime<Utc>>,
}

async fn lock_live_account(
    connection: &mut sqlx::PgConnection,
    user: Uuid,
) -> Result<bool, DatabaseError> {
    let row: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE",
    ).bind(user).fetch_optional(connection).await.map_err(map_sqlx_error)?;
    Ok(row.is_some())
}

async fn lock_admin(
    connection: &mut sqlx::PgConnection,
    admin: &AuthenticatedAdmin,
) -> Result<(), DatabaseError> {
    // Account, credential, then session: suspension, demotion and WebAuthn
    // revocation must serialize with the sensitive operation.
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE",
    ).bind(admin.user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
    if role.as_deref() != Some(admin.role.as_str()) {
        return Err(DatabaseError::AccountUnavailable);
    }
    let credential: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM admin_webauthn_credential WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL FOR SHARE",
    ).bind(admin.credential_id).bind(admin.user_id).fetch_optional(&mut *connection).await.map_err(map_sqlx_error)?;
    if credential.is_none() {
        return Err(DatabaseError::AccountUnavailable);
    }
    let session: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM admin_session WHERE id=$1 AND user_id=$2 AND credential_id=$3
           AND revoked_at IS NULL AND idle_expires_at > clock_timestamp()
           AND absolute_expires_at > clock_timestamp() FOR SHARE",
    )
    .bind(admin.session_id)
    .bind(admin.user_id)
    .bind(admin.credential_id)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?;
    session.map(|_| ()).ok_or(DatabaseError::AccountUnavailable)
}

async fn audit(
    connection: &mut sqlx::PgConnection,
    user: Uuid,
    admin: &AuthenticatedAdmin,
    action: &str,
    reason: &str,
) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO data_access_log(accessed_user_id,accessor_id,accessor_role,action,reason)
                 VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(user)
    .bind(admin.user_id)
    .bind(admin.role.as_str())
    .bind(action)
    .bind(reason)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}
