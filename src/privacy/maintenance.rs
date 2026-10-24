use chrono::{DateTime, Utc};

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};

const PRIVACY_MAINTENANCE_LOCK: i64 = 61_202_608;
pub const PRIVACY_MAINTENANCE_BATCH_SIZE: u32 = 1_000;
pub const PRIVACY_MAINTENANCE_MAX_BATCHES: u32 = 100;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrivacyMaintenanceResult {
    pub stale_presences: u64,
    pub expired_presences: u64,
    pub expired_swipes: u64,
    pub expired_otps: u64,
    pub expired_mobile_otp_requests: u64,
    pub expired_sms_attempts: u64,
    pub expired_web_challenges: u64,
    pub expired_web_sessions: u64,
    pub expired_store_notifications: u64,
    pub expired_store_subscriptions: u64,
    pub expired_store_accounts: u64,
    pub expired_refresh_tokens: u64,
    pub expired_notifications: u64,
    pub expired_consents: u64,
    pub expired_data_subject_requests: u64,
    pub expired_data_access_logs: u64,
    pub expired_reports: u64,
    pub expired_account_tombstones: u64,
    pub expired_account_deletion_tokens: u64,
    pub expired_admin_webauthn_challenges: u64,
    pub expired_admin_webauthn_bootstraps: u64,
    pub expired_admin_sessions: u64,
    pub expired_admin_auth_events: u64,
    pub expired_outbox_operator_actions: u64,
    pub expired_mobile_sessions: u64,
    pub expired_reward_intents: u64,
    pub expired_continuation_reservations: u64,
    pub expired_continuation_usage: u64,
}

impl PrivacyMaintenanceResult {
    pub fn processed(self) -> u64 {
        self.counts().into_iter().fold(0_u64, u64::saturating_add)
    }

    fn maximum(self) -> u64 {
        self.counts().into_iter().max().unwrap_or_default()
    }

    fn merge(&mut self, next: Self) {
        self.stale_presences = self.stale_presences.saturating_add(next.stale_presences);
        self.expired_presences = self
            .expired_presences
            .saturating_add(next.expired_presences);
        self.expired_swipes = self.expired_swipes.saturating_add(next.expired_swipes);
        self.expired_otps = self.expired_otps.saturating_add(next.expired_otps);
        self.expired_mobile_otp_requests = self.expired_mobile_otp_requests.saturating_add(next.expired_mobile_otp_requests);
        self.expired_sms_attempts = self
            .expired_sms_attempts
            .saturating_add(next.expired_sms_attempts);
        self.expired_refresh_tokens = self
            .expired_refresh_tokens
            .saturating_add(next.expired_refresh_tokens);
        self.expired_notifications = self
            .expired_notifications
            .saturating_add(next.expired_notifications);
        self.expired_consents = self.expired_consents.saturating_add(next.expired_consents);
        self.expired_data_subject_requests = self
            .expired_data_subject_requests
            .saturating_add(next.expired_data_subject_requests);
        self.expired_data_access_logs = self
            .expired_data_access_logs
            .saturating_add(next.expired_data_access_logs);
        self.expired_reports = self.expired_reports.saturating_add(next.expired_reports);
        self.expired_account_tombstones = self
            .expired_account_tombstones
            .saturating_add(next.expired_account_tombstones);
        self.expired_account_deletion_tokens = self
            .expired_account_deletion_tokens
            .saturating_add(next.expired_account_deletion_tokens);
        self.expired_admin_webauthn_challenges = self
            .expired_admin_webauthn_challenges
            .saturating_add(next.expired_admin_webauthn_challenges);
        self.expired_admin_webauthn_bootstraps = self
            .expired_admin_webauthn_bootstraps
            .saturating_add(next.expired_admin_webauthn_bootstraps);
        self.expired_admin_sessions = self
            .expired_admin_sessions
            .saturating_add(next.expired_admin_sessions);
        self.expired_admin_auth_events = self
            .expired_admin_auth_events
            .saturating_add(next.expired_admin_auth_events);
        self.expired_outbox_operator_actions = self
            .expired_outbox_operator_actions
            .saturating_add(next.expired_outbox_operator_actions);
        self.expired_reward_intents = self
            .expired_reward_intents
            .saturating_add(next.expired_reward_intents);
        self.expired_continuation_reservations = self
            .expired_continuation_reservations
            .saturating_add(next.expired_continuation_reservations);
        self.expired_continuation_usage = self
            .expired_continuation_usage
            .saturating_add(next.expired_continuation_usage);
        self.expired_mobile_sessions = self
            .expired_mobile_sessions
            .saturating_add(next.expired_mobile_sessions);
        self.expired_web_challenges = self
            .expired_web_challenges
            .saturating_add(next.expired_web_challenges);
        self.expired_web_sessions = self
            .expired_web_sessions
            .saturating_add(next.expired_web_sessions);
        self.expired_store_notifications = self
            .expired_store_notifications
            .saturating_add(next.expired_store_notifications);
        self.expired_store_subscriptions = self
            .expired_store_subscriptions
            .saturating_add(next.expired_store_subscriptions);
        self.expired_store_accounts = self
            .expired_store_accounts
            .saturating_add(next.expired_store_accounts);
    }

    fn counts(self) -> [u64; 28] {
        [
            self.stale_presences,
            self.expired_presences,
            self.expired_swipes,
            self.expired_otps,
            self.expired_refresh_tokens,
            self.expired_notifications,
            self.expired_consents,
            self.expired_data_subject_requests,
            self.expired_data_access_logs,
            self.expired_reports,
            self.expired_account_tombstones,
            self.expired_account_deletion_tokens,
            self.expired_admin_webauthn_challenges,
            self.expired_admin_webauthn_bootstraps,
            self.expired_admin_sessions,
            self.expired_admin_auth_events,
            self.expired_outbox_operator_actions,
            self.expired_mobile_sessions,
            self.expired_reward_intents,
            self.expired_continuation_reservations,
            self.expired_continuation_usage,
            self.expired_sms_attempts,
            self.expired_web_challenges,
            self.expired_web_sessions,
            self.expired_store_notifications,
            self.expired_store_subscriptions,
            self.expired_store_accounts,
            self.expired_mobile_otp_requests,
        ]
    }
}

#[derive(Clone)]
pub struct PrivacyMaintenanceRepository {
    database: Database,
}

impl PrivacyMaintenanceRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn run_as_leader(
        &self,
        now: DateTime<Utc>,
        batch_size: u32,
    ) -> Result<Option<PrivacyMaintenanceResult>, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
                        .bind(PRIVACY_MAINTENANCE_LOCK)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                    if !acquired {
                        return Ok(None);
                    }
                    let limit = i64::from(batch_size);
                    let mut counts = [0_u64; 28];
                    for (index, statement) in RETENTION_QUERIES.iter().enumerate() {
                        counts[index] = sqlx::query(statement)
                            .bind(now)
                            .bind(limit)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?
                            .rows_affected();
                    }
                    Ok(Some(PrivacyMaintenanceResult {
                        stale_presences: counts[0],
                        expired_presences: counts[1],
                        expired_swipes: counts[2],
                        expired_otps: counts[3],
                        expired_mobile_otp_requests: counts[27],
                        expired_refresh_tokens: counts[4],
                        expired_notifications: counts[5],
                        expired_consents: counts[6],
                        expired_data_subject_requests: counts[7],
                        expired_data_access_logs: counts[8],
                        expired_reports: counts[9],
                        expired_account_tombstones: counts[10],
                        expired_account_deletion_tokens: counts[11],
                        expired_admin_webauthn_challenges: counts[12],
                        expired_admin_webauthn_bootstraps: counts[13],
                        expired_admin_sessions: counts[14],
                        expired_admin_auth_events: counts[15],
                        expired_outbox_operator_actions: counts[16],
                        expired_mobile_sessions: counts[17],
                        expired_reward_intents: counts[18],
                        expired_continuation_reservations: counts[19],
                        expired_continuation_usage: counts[20],
                        expired_sms_attempts: counts[21],
                        expired_web_challenges: counts[22],
                        expired_web_sessions: counts[23],
                        expired_store_notifications: counts[24],
                        expired_store_subscriptions: counts[25],
                        expired_store_accounts: counts[26],
                    }))
                })
            })
            .await
    }
}

const RETENTION_QUERIES: [&str; 28] = [
    "WITH stale AS (
       SELECT user_id FROM user_presence
       WHERE is_location_fresh = true AND updated_at <= $1 - INTERVAL '1 hour'
       ORDER BY updated_at LIMIT $2
     ) UPDATE user_presence SET is_location_fresh = false
       WHERE user_id IN (SELECT user_id FROM stale)",
    "DELETE FROM user_presence WHERE user_id IN (
       SELECT user_id FROM user_presence WHERE updated_at <= $1 - INTERVAL '24 hours'
       ORDER BY updated_at LIMIT $2)",
    "DELETE FROM swipe_decision WHERE (actor_id, target_id) IN (
       SELECT actor_id, target_id FROM swipe_decision WHERE expires_at <= $1
       ORDER BY expires_at, actor_id, target_id LIMIT $2)",
    "DELETE FROM otp_verification WHERE id IN (
       SELECT id FROM otp_verification WHERE expires_at <= $1 ORDER BY expires_at LIMIT $2)",
    "DELETE FROM refresh_tokens WHERE id IN (
       SELECT id FROM refresh_tokens WHERE expires_at <= $1 ORDER BY expires_at LIMIT $2)",
    "DELETE FROM notification WHERE id IN (
       SELECT id FROM notification WHERE expires_at <= $1 ORDER BY expires_at LIMIT $2)",
    "DELETE FROM user_consent WHERE id IN (
       SELECT id FROM user_consent
       WHERE withdrawn_at IS NOT NULL AND withdrawn_at <= $1 - INTERVAL '5 years'
       ORDER BY withdrawn_at LIMIT $2)",
    "DELETE FROM data_subject_request WHERE id IN (
       SELECT id FROM data_subject_request
       WHERE status IN ('completed', 'rejected') AND completed_at IS NOT NULL
         AND completed_at <= $1 - INTERVAL '5 years'
       ORDER BY completed_at LIMIT $2)",
    "DELETE FROM data_access_log WHERE id IN (
       SELECT id FROM data_access_log WHERE accessed_at <= $1 - INTERVAL '1 year'
       ORDER BY accessed_at LIMIT $2)",
    "DELETE FROM user_report WHERE id IN (
       SELECT id FROM user_report
       WHERE status IN ('reviewed', 'dismissed') AND resolved_at IS NOT NULL
         AND resolved_at <= $1 - INTERVAL '3 years'
       ORDER BY resolved_at LIMIT $2)",
    "DELETE FROM account_tombstone WHERE phone_number_hash IN (
       SELECT phone_number_hash FROM account_tombstone WHERE expires_at <= $1
       ORDER BY expires_at LIMIT $2)",
    "DELETE FROM account_deletion_token WHERE id IN (
       SELECT id FROM account_deletion_token WHERE expires_at <= $1
       ORDER BY expires_at LIMIT $2)",
    "DELETE FROM admin_webauthn_challenge WHERE id IN (
       SELECT id FROM (
         (SELECT id, expires_at FROM admin_webauthn_challenge
          WHERE consumed_at IS NOT NULL ORDER BY expires_at, id LIMIT $2)
         UNION ALL
         (SELECT id, expires_at FROM admin_webauthn_challenge
          WHERE consumed_at IS NULL AND expires_at <= $1
          ORDER BY expires_at, id LIMIT $2)
       ) AS candidates ORDER BY expires_at, id LIMIT $2)",
    "DELETE FROM admin_webauthn_bootstrap WHERE id IN (
       SELECT id FROM (
         (SELECT id, expires_at FROM admin_webauthn_bootstrap
          WHERE consumed_at IS NOT NULL ORDER BY expires_at, id LIMIT $2)
         UNION ALL
         (SELECT id, expires_at FROM admin_webauthn_bootstrap
          WHERE consumed_at IS NULL AND expires_at <= $1
          ORDER BY expires_at, id LIMIT $2)
       ) AS candidates ORDER BY expires_at, id LIMIT $2)",
    "DELETE FROM admin_session WHERE id IN (
       SELECT id FROM (
         (SELECT id, absolute_expires_at AS cleanup_at FROM admin_session
          WHERE absolute_expires_at <= $1 ORDER BY absolute_expires_at, id LIMIT $2)
         UNION ALL
         (SELECT id, revoked_at AS cleanup_at FROM admin_session
          WHERE revoked_at <= $1 - INTERVAL '24 hours' AND absolute_expires_at > $1
          ORDER BY revoked_at, id LIMIT $2)
       ) AS candidates ORDER BY cleanup_at, id LIMIT $2)",
    "DELETE FROM admin_auth_event WHERE id IN (
       SELECT id FROM admin_auth_event WHERE created_at <= $1 - INTERVAL '1 year'
       ORDER BY created_at LIMIT $2)",
    "DELETE FROM outbox_operator_action WHERE id IN (
       SELECT id FROM outbox_operator_action WHERE created_at <= $1 - INTERVAL '1 year'
       ORDER BY created_at LIMIT $2)",
    "DELETE FROM refresh_token_family WHERE id IN (
       SELECT family.id FROM refresh_token_family AS family
       WHERE family.expires_at <= $1
         AND NOT EXISTS (SELECT 1 FROM refresh_tokens WHERE family_id = family.id)
       ORDER BY family.expires_at, family.id LIMIT $2)",
    "DELETE FROM advertising_reward_intent WHERE id IN (SELECT id FROM advertising_reward_intent WHERE created_at <= $1 - INTERVAL '90 days' ORDER BY created_at,id LIMIT $2)",
    "DELETE FROM continuation_reservation WHERE (match_id,user_id) IN (SELECT r.match_id,r.user_id FROM continuation_reservation r JOIN match_init m ON m.id=r.match_id WHERE m.status<>'awaiting_continuation' OR m.expires_at<=$1 ORDER BY m.expires_at,r.match_id,r.user_id LIMIT $2 FOR UPDATE OF m SKIP LOCKED)",
    "DELETE FROM continuation_usage WHERE (user_id,week_start) IN (SELECT u.user_id,u.week_start FROM continuation_usage u WHERE u.week_start < ($1 AT TIME ZONE 'UTC')::date - 90 AND NOT EXISTS(SELECT 1 FROM continuation_reservation r WHERE r.user_id=u.user_id AND r.week_start=u.week_start) ORDER BY u.week_start,u.user_id LIMIT $2)",
    "DELETE FROM sms_send_attempt WHERE delivery_id IN (
       SELECT delivery_id FROM sms_send_attempt WHERE reserved_at <= $1 - interval '25 hours'
       ORDER BY reserved_at LIMIT $2)",
    "DELETE FROM web_billing_challenge WHERE id IN (SELECT id FROM web_billing_challenge WHERE expires_at <= $1 ORDER BY expires_at,id LIMIT $2)",
    "DELETE FROM web_billing_session WHERE token_hash IN (SELECT token_hash FROM web_billing_session WHERE expires_at <= $1 ORDER BY expires_at,token_hash LIMIT $2)",
    "DELETE FROM billing_store_notification WHERE id IN (SELECT id FROM billing_store_notification WHERE processed_at <= $1 - interval '7 days' OR received_at <= $1 - interval '90 days' ORDER BY received_at,id LIMIT $2)",
    "DELETE FROM billing_store_subscription WHERE id IN (SELECT id FROM billing_store_subscription WHERE expires_at <= $1 - interval '400 days' AND next_check_at <= $1 ORDER BY expires_at,id LIMIT $2)",
    "DELETE FROM billing_store_account WHERE id IN (SELECT a.id FROM billing_store_account a WHERE a.user_id IS NULL AND a.created_at <= $1 - interval '400 days' AND NOT EXISTS(SELECT 1 FROM billing_store_subscription s WHERE s.account_id=a.id) ORDER BY a.created_at,a.id LIMIT $2)",
    "DELETE FROM mobile_otp_request WHERE id IN (
       SELECT id FROM mobile_otp_request WHERE expires_at <= $1 AND resend_at <= $1
       ORDER BY expires_at,id LIMIT $2)",
];

#[derive(Clone)]
pub struct PrivacyMaintenanceService {
    repository: PrivacyMaintenanceRepository,
    tracker: MaintenanceTracker,
}

impl PrivacyMaintenanceService {
    pub fn new(repository: PrivacyMaintenanceRepository, tracker: MaintenanceTracker) -> Self {
        Self {
            repository,
            tracker,
        }
    }

    pub async fn run_once(&self) -> Result<Option<PrivacyMaintenanceRun>, DatabaseError> {
        self.tracker
            .track(
                MaintenanceJobName::Privacy,
                self.perform(),
                |run| MaintenanceProgress {
                    processed_count: run.result.processed(),
                    batch_count: run.batch_count,
                    work_remaining: run.work_remaining,
                },
                |_| "privacy_maintenance_failed",
            )
            .await
    }

    async fn perform(&self) -> Result<Option<PrivacyMaintenanceRun>, DatabaseError> {
        let mut totals = PrivacyMaintenanceResult::default();
        let mut completed_batches = 0_u32;
        for _ in 0..PRIVACY_MAINTENANCE_MAX_BATCHES {
            let Some(batch) = self
                .repository
                .run_as_leader(Utc::now(), PRIVACY_MAINTENANCE_BATCH_SIZE)
                .await?
            else {
                return if completed_batches == 0 {
                    Ok(None)
                } else {
                    Ok(Some(PrivacyMaintenanceRun {
                        result: totals,
                        batch_count: completed_batches,
                        work_remaining: true,
                    }))
                };
            };
            completed_batches = completed_batches.saturating_add(1);
            let full = batch.maximum() >= u64::from(PRIVACY_MAINTENANCE_BATCH_SIZE);
            totals.merge(batch);
            if !full {
                return Ok(Some(PrivacyMaintenanceRun {
                    result: totals,
                    batch_count: completed_batches,
                    work_remaining: false,
                }));
            }
        }
        Ok(Some(PrivacyMaintenanceRun {
            result: totals,
            batch_count: completed_batches,
            work_remaining: true,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivacyMaintenanceRun {
    pub result: PrivacyMaintenanceResult,
    pub batch_count: u32,
    pub work_remaining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_every_retention_counter_without_overflow() {
        let result = PrivacyMaintenanceResult {
            stale_presences: 1,
            expired_presences: 2,
            expired_swipes: 3,
            ..PrivacyMaintenanceResult::default()
        };
        assert_eq!(result.processed(), 6);
        assert_eq!(result.maximum(), 3);
        assert_eq!(RETENTION_QUERIES.len(), 28);
    }
}
