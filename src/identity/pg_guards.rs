//! Account and consent locks shared by transactional user actions.
use crate::{
    config::LegalConfig,
    infra::postgres::{DatabaseError, map_sqlx_error},
};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

/// Serialize pair creation and blocking before either transaction locks a match.
/// This also covers a match row that is not visible until its creator commits.
pub(crate) async fn lock_user_pair(
    connection: &mut PgConnection,
    first: Uuid,
    second: Uuid,
) -> Result<(), DatabaseError> {
    let (first, second) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 86302005))")
        .bind(format!("{first}:{second}"))
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

pub(crate) async fn lock_eligible_accounts(
    connection: &mut PgConnection,
    users: &[Uuid],
    legal: Option<&LegalConfig>,
) -> Result<(), DatabaseError> {
    let accounts = sqlx::query(
        "SELECT user_id,role,is_banned,deleted_at
         FROM user_account
         WHERE user_id=ANY($1)
         ORDER BY user_id FOR SHARE",
    )
    .bind(users)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if accounts.len() != users.len() {
        return Err(DatabaseError::AccountUnavailable);
    }
    for account in accounts {
        if account
            .try_get::<bool, _>("is_banned")
            .map_err(map_sqlx_error)?
            || account
                .try_get::<Option<DateTime<Utc>>, _>("deleted_at")
                .map_err(map_sqlx_error)?
                .is_some()
        {
            return Err(DatabaseError::AccountUnavailable);
        }
        if let Some(legal) = legal
            && account
                .try_get::<String, _>("role")
                .map_err(map_sqlx_error)?
                == "user"
        {
            let id: Uuid = account.try_get("user_id").map_err(map_sqlx_error)?;
            let consents = sqlx::query(
                "SELECT consent_type
                 FROM user_consent
                 WHERE user_id=$1 AND granted AND withdrawn_at IS NULL AND ((consent_type='terms_of_service_acceptance' AND document_version=$2) OR (consent_type='privacy_notice_acknowledgement' AND document_version=$3)) FOR SHARE",
            )
        .bind(id)
        .bind(&legal.terms_version)
        .bind(&legal.privacy_version)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
            let mut kinds = std::collections::BTreeSet::new();
            for consent in consents {
                kinds.insert(
                    consent
                        .try_get::<String, _>("consent_type")
                        .map_err(map_sqlx_error)?,
                );
            }
            if kinds.len() != 2 {
                return Err(DatabaseError::AccountUnavailable);
            }
        }
    }
    Ok(())
}
