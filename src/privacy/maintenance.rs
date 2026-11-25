use chrono::{DateTime, Utc};

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};

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
    pub expired_refresh_recoveries: u64,
    pub expired_phone_observations: u64,
    pub expired_phone_policies: u64,
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
    pub expired_support_cases: u64,
    pub expired_erasure_receipts: u64,
    pub expired_assisted_exports: u64,
}

impl PrivacyMaintenanceResult {
    pub fn processed(self) -> u64 {
        self.counts().into_iter().fold(0_u64, u64::saturating_add)
    }

    fn maximum(self) -> u64 {
        self.counts().into_iter().max().unwrap_or_default()
    }

    fn merge(&mut self, next: Self) {
        self.expired_support_cases = self
            .expired_support_cases
            .saturating_add(next.expired_support_cases);
        self.expired_erasure_receipts = self
            .expired_erasure_receipts
            .saturating_add(next.expired_erasure_receipts);
        self.expired_assisted_exports = self
            .expired_assisted_exports
            .saturating_add(next.expired_assisted_exports);
        self.expired_phone_observations = self
            .expired_phone_observations
            .saturating_add(next.expired_phone_observations);
        self.expired_phone_policies = self
            .expired_phone_policies
            .saturating_add(next.expired_phone_policies);
        self.stale_presences = self.stale_presences.saturating_add(next.stale_presences);
        self.expired_presences = self
            .expired_presences
            .saturating_add(next.expired_presences);
        self.expired_swipes = self.expired_swipes.saturating_add(next.expired_swipes);
        self.expired_otps = self.expired_otps.saturating_add(next.expired_otps);
        self.expired_mobile_otp_requests = self
            .expired_mobile_otp_requests
            .saturating_add(next.expired_mobile_otp_requests);
        self.expired_sms_attempts = self
            .expired_sms_attempts
            .saturating_add(next.expired_sms_attempts);
        self.expired_refresh_tokens = self
            .expired_refresh_tokens
            .saturating_add(next.expired_refresh_tokens);
        self.expired_refresh_recoveries = self
            .expired_refresh_recoveries
            .saturating_add(next.expired_refresh_recoveries);
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

    fn counts(self) -> [u64; 34] {
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
            self.expired_refresh_recoveries,
            self.expired_phone_observations,
            self.expired_phone_policies,
            self.expired_support_cases,
            self.expired_erasure_receipts,
            self.expired_assisted_exports,
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
        let counts: Option<Vec<i64>> =
            sqlx::query_scalar("SELECT fct_run_privacy_retention($1, $2)")
                .bind(now)
                .bind(i64::from(batch_size))
                .fetch_one(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
        let Some(counts) = counts else {
            return Ok(None);
        };
        let counts: [i64; 34] = counts.try_into().map_err(|_| DatabaseError::QueryFailed)?;
        Ok(Some(PrivacyMaintenanceResult {
            stale_presences: counts[0] as u64,
            expired_presences: counts[1] as u64,
            expired_swipes: counts[2] as u64,
            expired_phone_observations: counts[29] as u64,
            expired_phone_policies: counts[30] as u64,
            expired_support_cases: counts[31] as u64,
            expired_erasure_receipts: counts[32] as u64,
            expired_assisted_exports: counts[33] as u64,
            expired_otps: counts[3] as u64,
            expired_mobile_otp_requests: counts[27] as u64,
            expired_refresh_tokens: counts[4] as u64,
            expired_refresh_recoveries: counts[28] as u64,
            expired_notifications: counts[5] as u64,
            expired_consents: counts[6] as u64,
            expired_data_subject_requests: counts[7] as u64,
            expired_data_access_logs: counts[8] as u64,
            expired_reports: counts[9] as u64,
            expired_account_tombstones: counts[10] as u64,
            expired_account_deletion_tokens: counts[11] as u64,
            expired_admin_webauthn_challenges: counts[12] as u64,
            expired_admin_webauthn_bootstraps: counts[13] as u64,
            expired_admin_sessions: counts[14] as u64,
            expired_admin_auth_events: counts[15] as u64,
            expired_outbox_operator_actions: counts[16] as u64,
            expired_mobile_sessions: counts[17] as u64,
            expired_reward_intents: counts[18] as u64,
            expired_continuation_reservations: counts[19] as u64,
            expired_continuation_usage: counts[20] as u64,
            expired_sms_attempts: counts[21] as u64,
            expired_web_challenges: counts[22] as u64,
            expired_web_sessions: counts[23] as u64,
            expired_store_notifications: counts[24] as u64,
            expired_store_subscriptions: counts[25] as u64,
            expired_store_accounts: counts[26] as u64,
        }))
    }
}

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
    fn reports_total_and_largest_retention_counts() {
        let result = PrivacyMaintenanceResult {
            stale_presences: 1,
            expired_presences: 2,
            expired_swipes: 3,
            ..PrivacyMaintenanceResult::default()
        };
        assert_eq!(result.processed(), 6);
        assert_eq!(result.maximum(), 3);
    }
}
