use super::{
    StoreFinanceImport,
    domain::{
        StoreAvailability, StoreBillingStatus, StoreFinanceSummary, StoreSource,
        StoreSubscriptionSummary,
    },
};
use crate::billing::{domain::BillingPeriod, stores::StoreProvider};
use crate::{
    administration::metrics::RevenuePeriod,
    billing::stores::{StoreError, pg::StoreRepository},
    config::StoreBillingConfig,
    infra::postgres::{DatabaseError, map_sqlx_error},
};
use sqlx::Row;
use uuid::Uuid;

const SOURCES_SQL: &str = r#"
SELECT provider, count(*) AS subscriptions, count(DISTINCT user_id) FILTER(WHERE
starts_at<=clock_timestamp() AND expires_at>clock_timestamp() AND (status IN ('active',
'grace_period') OR (provider='google_play' AND status='canceled'))) AS active_accounts,
count(*) FILTER(WHERE NOT acknowledged AND user_id IS NOT NULL) AS unacknowledged,
max(verified_at) AS last_verified_at FROM billing_store_subscription GROUP BY provider
ORDER BY provider
"#;

const RECENT_SQL: &str = r#"
SELECT user_id, provider, billing_period, status, expires_at, auto_renew, verified_at FROM
billing_store_subscription WHERE user_id IS NOT NULL ORDER BY verified_at DESC, id DESC
LIMIT 50
"#;

const PENDING_SQL: &str = r#"
SELECT count(*) FROM billing_store_notification WHERE processed_at IS NULL
"#;

const DEAD_LETTERS_SQL: &str = r#"
SELECT count(*) FROM outbox_event WHERE event_type LIKE 'billing.store.%' AND status='dead_letter'
"#;

const LOCK_IMPORT_ACTOR_SQL: &str = r#"
SELECT role FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE
"#;

const IMPORT_ROW_SQL: &str = r#"
INSERT INTO billing_store_finance(provider, report_id, row_id, paid_at, currency,
gross_cents, net_cents, imported_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8) ON
CONFLICT(provider, report_id, row_id) DO UPDATE SET row_id=EXCLUDED.row_id WHERE
billing_store_finance.paid_at=EXCLUDED.paid_at AND
billing_store_finance.currency=EXCLUDED.currency AND
billing_store_finance.gross_cents=EXCLUDED.gross_cents AND billing_store_finance.net_cents
IS NOT DISTINCT FROM EXCLUDED.net_cents
"#;

const AUDIT_IMPORT_SQL: &str = r#"
INSERT INTO data_access_log(accessed_user_id, accessor_id, accessor_role, action, reason)
VALUES($1, $1, 'superadmin', 'admin_import_store_finance',
'Imported store financial report')
"#;

const FINANCE_SQL: &str = r#"
SELECT finance.provider, finance.currency, sum(finance.gross_cents)::bigint AS gross_cents,
       CASE WHEN count(*)=count(finance.net_cents)
            THEN sum(finance.net_cents)::bigint END AS net_cents,
       count(*) AS rows, max(finance.imported_at) AS last_imported_at
FROM billing_store_finance finance CROSS JOIN bounds
WHERE (bounds.period_start IS NULL OR finance.paid_at>=bounds.period_start)
  AND finance.paid_at<bounds.period_end
GROUP BY finance.provider, finance.currency
ORDER BY finance.provider, finance.currency
"#;

pub(super) async fn snapshot(
    repository: &StoreRepository,
    config: &StoreBillingConfig,
    period: RevenuePeriod,
) -> Result<StoreBillingStatus, StoreError> {
    let db = repository.database.pool();
    let sources = sqlx::query(SOURCES_SQL)
        .fetch_all(db)
        .await
        .map_err(map_sqlx_error)?;
    let sources = sources
        .iter()
        .map(|row| {
            Ok::<_, StoreError>(StoreSource {
                provider: StoreProvider::parse(
                    &row.try_get::<String, _>("provider")
                        .map_err(map_sqlx_error)?,
                )?,
                subscriptions: row.try_get("subscriptions").map_err(map_sqlx_error)?,
                active_accounts: row.try_get("active_accounts").map_err(map_sqlx_error)?,
                unacknowledged: row.try_get("unacknowledged").map_err(map_sqlx_error)?,
                last_verified_at: row.try_get("last_verified_at").map_err(map_sqlx_error)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sql = format!(
        "{} {}",
        crate::administration::metrics::REVENUE_CTES_SQL,
        FINANCE_SQL
    );
    let finance = sqlx::query(&sql)
        .bind(period.as_str())
        .fetch_all(db)
        .await
        .map_err(map_sqlx_error)?;
    let finance = finance
        .iter()
        .map(|row| {
            Ok::<_, StoreError>(StoreFinanceSummary {
                provider: StoreProvider::parse(
                    &row.try_get::<String, _>("provider")
                        .map_err(map_sqlx_error)?,
                )?,
                currency: row.try_get("currency").map_err(map_sqlx_error)?,
                gross_cents: row.try_get("gross_cents").map_err(map_sqlx_error)?,
                net_cents: row.try_get("net_cents").map_err(map_sqlx_error)?,
                rows: row.try_get("rows").map_err(map_sqlx_error)?,
                last_imported_at: row.try_get("last_imported_at").map_err(map_sqlx_error)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let recent = sqlx::query(RECENT_SQL)
        .fetch_all(db)
        .await
        .map_err(map_sqlx_error)?;
    let recent = recent
        .iter()
        .map(|row| {
            Ok::<_, StoreError>(StoreSubscriptionSummary {
                user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
                provider: StoreProvider::parse(
                    &row.try_get::<String, _>("provider")
                        .map_err(map_sqlx_error)?,
                )?,
                billing_period: BillingPeriod::parse(
                    &row.try_get::<String, _>("billing_period")
                        .map_err(map_sqlx_error)?,
                )
                .ok_or(StoreError::Invalid)?,
                status: row.try_get("status").map_err(map_sqlx_error)?,
                expires_at: row.try_get("expires_at").map_err(map_sqlx_error)?,
                auto_renew: row.try_get("auto_renew").map_err(map_sqlx_error)?,
                verified_at: row.try_get("verified_at").map_err(map_sqlx_error)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pending: i64 = sqlx::query_scalar(PENDING_SQL)
        .fetch_one(db)
        .await
        .map_err(map_sqlx_error)?;
    let dead: i64 = sqlx::query_scalar(DEAD_LETTERS_SQL)
        .fetch_one(db)
        .await
        .map_err(map_sqlx_error)?;
    Ok(StoreBillingStatus {
        enabled: StoreAvailability {
            app_store: config.apple_enabled,
            google_play: config.google_enabled,
        },
        sources,
        period,
        finance,
        recent,
        pending_notifications: pending,
        dead_letters: dead,
        finance_basis: "imported_store_reports",
    })
}
#[derive(Debug)]
pub enum FinanceImportError {
    Invalid,
    Forbidden,
    Conflict,
    Database(DatabaseError),
}
impl From<DatabaseError> for FinanceImportError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}
pub async fn import_report(
    repository: &StoreRepository,
    actor: Uuid,
    body: StoreFinanceImport,
) -> Result<(), FinanceImportError> {
    if !body.valid() {
        return Err(FinanceImportError::Invalid);
    }
    repository
        .database
        .transaction(|connection| {
            Box::pin(async move {
                let role: Option<String> = sqlx::query_scalar(LOCK_IMPORT_ACTOR_SQL)
                    .bind(actor)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                if role.as_deref() != Some("superadmin") {
                    return Err(FinanceImportError::Forbidden);
                }
                let mut changed = 0;
                for row in body.rows {
                    let result = sqlx::query(IMPORT_ROW_SQL)
                        .bind(body.provider.as_str())
                        .bind(&body.report_id)
                        .bind(row.row_id)
                        .bind(row.paid_at)
                        .bind(row.currency)
                        .bind(row.gross_cents)
                        .bind(row.net_cents)
                        .bind(actor)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if result.rows_affected() != 1 {
                        return Err(FinanceImportError::Conflict);
                    }
                    changed += 1;
                }
                if changed > 0 {
                    sqlx::query(AUDIT_IMPORT_SQL)
                        .bind(actor)
                        .execute(connection)
                        .await
                        .map_err(map_sqlx_error)?;
                }
                Ok(())
            })
        })
        .await
}
