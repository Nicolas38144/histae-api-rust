use uuid::Uuid;

use crate::administration::domain::{IdentityCorrectionResult, ProfileIdentityCorrection};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use crate::profiles::domain::Sex;

impl super::PgAdministrationStore {
    pub(super) async fn correct_identity(
        &self,
        target_id: Uuid,
        correction: ProfileIdentityCorrection,
        reason: String,
        actor_id: Uuid,
        actor_role: AdminRole,
        sensitive_version: String,
    ) -> Result<IdentityCorrectionResult, DatabaseError> {
        self.database
            .transaction(move |connection| {
                Box::pin(async move {
                    // Serialize with profile creation, consent withdrawal, deletion and
                    // role changes. Stable ordering also covers corrections of oneself.
                    let accounts = sqlx::query_as::<_, (Uuid, String, bool)>(
                        "SELECT user_id, role, is_banned FROM user_account
                     WHERE user_id = ANY($1::uuid[]) AND deleted_at IS NULL
                     ORDER BY user_id FOR UPDATE",
                    )
                    .bind(vec![actor_id, target_id])
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some((_, current_role, is_banned)) =
                        accounts.iter().find(|(id, ..)| *id == actor_id)
                    else {
                        return Ok(IdentityCorrectionResult::Forbidden);
                    };
                    if current_role != actor_role.as_str() || *is_banned {
                        return Ok(IdentityCorrectionResult::Forbidden);
                    }
                    let Some((_, target_role, _)) =
                        accounts.iter().find(|(id, ..)| *id == target_id)
                    else {
                        return Ok(IdentityCorrectionResult::NotFound);
                    };
                    if actor_role == AdminRole::Admin && target_role != "user" {
                        return Ok(IdentityCorrectionResult::Forbidden);
                    }
                    let profile: Option<Uuid> = sqlx::query_scalar(
                        "SELECT user_id FROM user_profile WHERE user_id=$1 FOR UPDATE",
                    )
                    .bind(target_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if profile.is_none() {
                        return Ok(IdentityCorrectionResult::NotFound);
                    }
                    if correction.sex.is_some() {
                        let consent: bool = sqlx::query_scalar(
                            "SELECT EXISTS(SELECT 1 FROM user_consent
                         WHERE user_id=$1 AND consent_type='sensitive_data_consent'
                           AND granted=true AND withdrawn_at IS NULL AND document_version=$2)",
                        )
                        .bind(target_id)
                        .bind(sensitive_version)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if !consent {
                            return Ok(IdentityCorrectionResult::ConsentRequired);
                        }
                    }
                    sqlx::query(
                    "UPDATE user_profile SET firstname=$2, birthdate=$3, sex=$4 WHERE user_id=$1",
                )
                .bind(target_id)
                .bind(correction.firstname)
                .bind(correction.birthdate)
                .bind(correction.sex.map(Sex::as_str))
                .execute(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
                    // Do not duplicate birthdates or sex values in the audit trail.
                    super::record_audit(
                        connection,
                        target_id,
                        actor_id,
                        actor_role,
                        "admin_correct_profile_identity",
                        &reason,
                    )
                    .await?;
                    Ok(IdentityCorrectionResult::Updated)
                })
            })
            .await
    }
}
