//! Operator grants are independent of billing provider projections.

use sqlx::Row as _;
use uuid::Uuid;

use super::{PgAdministrationStore, record_audit};
use crate::{
    administration::domain::PremiumGrantResult,
    identity::admin_role::AdminRole,
    infra::postgres::{DatabaseError, map_sqlx_error},
    matches::pg::credits::lock_wallet,
};

impl PgAdministrationStore {
    pub(super) async fn set_premium_grant(
        &self,
        target_id: Uuid,
        enabled: bool,
        reason: String,
        actor_id: Uuid,
    ) -> Result<PremiumGrantResult, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    // Match confirmations use these same ordered locks before account rows.
                    for id in std::collections::BTreeSet::from([actor_id, target_id]) {
                        lock_wallet(connection, id).await?;
                    }
                    let actor: Option<Uuid> = sqlx::query_scalar(
                        "SELECT user_id FROM user_account WHERE user_id=$1
                         AND role='superadmin' AND NOT is_banned AND deleted_at IS NULL FOR SHARE",
                    )
                    .bind(actor_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if actor.is_none() {
                        return Ok(PremiumGrantResult::Forbidden);
                    }
                    let target = sqlx::query(
                        "SELECT is_banned,is_review_account FROM user_account
                         WHERE user_id=$1 AND deleted_at IS NULL FOR UPDATE",
                    )
                    .bind(target_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some(target) = target else {
                        return Ok(PremiumGrantResult::NotFound);
                    };
                    if target
                        .try_get::<bool, _>("is_review_account")
                        .map_err(map_sqlx_error)?
                        || (enabled
                            && target
                                .try_get::<bool, _>("is_banned")
                                .map_err(map_sqlx_error)?)
                    {
                        return Ok(PremiumGrantResult::Forbidden);
                    }
                    let changed = if enabled {
                        sqlx::query(
                            "INSERT INTO complimentary_premium_grant(user_id,granted_by)
                             VALUES($1,$2) ON CONFLICT(user_id) DO NOTHING",
                        )
                        .bind(target_id)
                        .bind(actor_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?
                        .rows_affected()
                    } else {
                        sqlx::query("DELETE FROM complimentary_premium_grant WHERE user_id=$1")
                            .bind(target_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?
                            .rows_affected()
                    };
                    if changed == 0 {
                        return Ok(PremiumGrantResult::Unchanged);
                    }
                    record_audit(
                        connection,
                        target_id,
                        actor_id,
                        AdminRole::Superadmin,
                        if enabled {
                            "admin_grant_premium"
                        } else {
                            "admin_revoke_premium"
                        },
                        &reason,
                    )
                    .await?;
                    Ok(PremiumGrantResult::Updated)
                })
            })
            .await
    }
}
