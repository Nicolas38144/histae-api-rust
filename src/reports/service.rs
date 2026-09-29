use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{PageCursor, PublicReport, ReportReason, ReportRecord, ReportStatus};
use super::pg::ReportStore;
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{ConstraintKind, DatabaseError};
use crate::shared::text::javascript_trim;

#[derive(Clone, Debug)]
pub struct CreateReportInput {
    pub reported_user_id: Uuid,
    pub match_id: Option<Uuid>,
    pub reason: ReportReason,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportError {
    InvalidRequest,
    InvalidCursor,
    AccountNotFound,
    MatchNotFound,
    AlreadyPending,
    ReportNotFound,
    Database(DatabaseError),
}

impl From<DatabaseError> for ReportError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportPage {
    pub items: Vec<PublicReport>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct ReportService {
    store: Arc<dyn ReportStore>,
}

impl ReportService {
    pub fn new(store: Arc<dyn ReportStore>) -> Self {
        Self { store }
    }

    pub async fn create(
        &self,
        reporter_id: Uuid,
        input: CreateReportInput,
    ) -> Result<PublicReport, ReportError> {
        if reporter_id == input.reported_user_id
            || input
                .description
                .as_ref()
                .is_some_and(|value| value.len() > 2_000)
        {
            return Err(ReportError::InvalidRequest);
        }
        if !self.store.account_exists(input.reported_user_id).await? {
            return Err(ReportError::AccountNotFound);
        }
        if let Some(match_id) = input.match_id {
            let participants = self.store.match_participants(match_id).await?;
            if !participants.is_some_and(|participants| {
                participants.contains(&reporter_id)
                    && participants.contains(&input.reported_user_id)
            }) {
                return Err(ReportError::MatchNotFound);
            }
        }
        let report = ReportRecord {
            id: Uuid::new_v4(),
            reporter_id,
            reported_id: input.reported_user_id,
            match_id: input.match_id,
            reason: input.reason,
            description: input.description.and_then(|value| {
                let trimmed = javascript_trim(&value);
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            }),
            status: ReportStatus::Pending,
            created_at: Utc::now(),
            resolved_at: None,
        };
        match self.store.create(report.clone()).await {
            Ok(()) => Ok(PublicReport::from(report)),
            Err(DatabaseError::Constraint(ConstraintKind::Unique)) => {
                Err(ReportError::AlreadyPending)
            }
            Err(error) => Err(ReportError::Database(error)),
        }
    }

    pub async fn list(
        &self,
        status: Option<ReportStatus>,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<ReportPage, ReportError> {
        if !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(ReportError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let rows = self.store.list(status, limit + 1, offset, cursor).await?;
        let has_more = rows.len() > limit as usize;
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.report.id))
                .transpose()?
        } else {
            None
        };
        Ok(ReportPage {
            items: rows
                .into_iter()
                .take(limit as usize)
                .map(|row| PublicReport::from(row.report))
                .collect(),
            next_cursor,
        })
    }

    pub async fn update_status(
        &self,
        id: Uuid,
        status: ReportStatus,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> Result<(), ReportError> {
        if !self
            .store
            .update_status(id, status, admin_id, admin_role)
            .await?
        {
            return Err(ReportError::ReportNotFound);
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, ReportError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.encode_utf16().count() > 512 {
        return Err(ReportError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ReportError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| ReportError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| ReportError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || cursor.id.len() != 36
        || id.hyphenated().to_string() != cursor.id.to_ascii_lowercase()
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != Variant::RFC4122
    {
        return Err(ReportError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| ReportError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, ReportError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| ReportError::InvalidCursor)
}

fn valid_cursor_timestamp(value: &str) -> bool {
    let Some((date, fraction)) = value
        .strip_suffix('Z')
        .and_then(|value| value.rsplit_once('.'))
    else {
        return false;
    };
    (3..=6).contains(&fraction.len())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && date.len() == 19
        && DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::TimeZone as _;

    use super::*;
    use crate::reports::domain::CursorReportRow;
    use crate::reports::pg::ReportStoreFuture;

    struct FakeStore {
        account_exists: bool,
        participants: Option<[Uuid; 2]>,
        create_error: Option<DatabaseError>,
        created: Mutex<Vec<ReportRecord>>,
    }

    impl Default for FakeStore {
        fn default() -> Self {
            Self {
                account_exists: true,
                participants: None,
                create_error: None,
                created: Mutex::new(Vec::new()),
            }
        }
    }

    impl ReportStore for FakeStore {
        fn account_exists(&self, _user_id: Uuid) -> ReportStoreFuture<'_, bool> {
            Box::pin(async move { Ok(self.account_exists) })
        }

        fn match_participants(&self, _match_id: Uuid) -> ReportStoreFuture<'_, Option<[Uuid; 2]>> {
            Box::pin(async move { Ok(self.participants) })
        }

        fn create(&self, report: ReportRecord) -> ReportStoreFuture<'_, ()> {
            Box::pin(async move {
                if let Some(error) = self.create_error {
                    return Err(error);
                }
                self.created
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(report);
                Ok(())
            })
        }

        fn list(
            &self,
            _status: Option<ReportStatus>,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> ReportStoreFuture<'_, Vec<CursorReportRow>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn update_status(
            &self,
            _id: Uuid,
            _status: ReportStatus,
            _admin_id: Uuid,
            _admin_role: AdminRole,
        ) -> ReportStoreFuture<'_, bool> {
            Box::pin(async { Ok(false) })
        }
    }

    fn input(target: Uuid) -> CreateReportInput {
        CreateReportInput {
            reported_user_id: target,
            match_id: None,
            reason: ReportReason::Harassment,
            description: Some("  Messages insistants.  ".to_owned()),
        }
    }

    #[tokio::test]
    async fn creates_generated_reports_and_omits_empty_optional_fields() {
        let store = Arc::new(FakeStore::default());
        let service = ReportService::new(store.clone());
        let reporter = Uuid::new_v4();
        let report = service
            .create(reporter, input(Uuid::new_v4()))
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(report.reporter_id, reporter);
        assert_eq!(report.description.as_deref(), Some("Messages insistants."));
        assert_eq!(report.status, ReportStatus::Pending);
        assert_eq!(report.id.get_version_num(), 4);
        assert!(store.created.lock().is_ok_and(|rows| rows.len() == 1));
    }

    #[tokio::test]
    async fn enforces_target_match_bytes_uniqueness_and_missing_update_contracts() {
        let user = Uuid::new_v4();
        let service = ReportService::new(Arc::new(FakeStore::default()));
        assert_eq!(
            service.create(user, input(user)).await,
            Err(ReportError::InvalidRequest)
        );

        let missing = ReportService::new(Arc::new(FakeStore {
            account_exists: false,
            ..FakeStore::default()
        }));
        assert_eq!(
            missing.create(user, input(Uuid::new_v4())).await,
            Err(ReportError::AccountNotFound)
        );

        let target = Uuid::new_v4();
        let mut matched_input = input(target);
        matched_input.match_id = Some(Uuid::new_v4());
        assert_eq!(
            service.create(user, matched_input).await,
            Err(ReportError::MatchNotFound)
        );

        let duplicate = ReportService::new(Arc::new(FakeStore {
            create_error: Some(DatabaseError::Constraint(ConstraintKind::Unique)),
            ..FakeStore::default()
        }));
        assert_eq!(
            duplicate.create(user, input(target)).await,
            Err(ReportError::AlreadyPending)
        );
        assert_eq!(
            service
                .update_status(
                    Uuid::new_v4(),
                    ReportStatus::Reviewed,
                    Uuid::new_v4(),
                    AdminRole::Admin,
                )
                .await,
            Err(ReportError::ReportNotFound)
        );
    }

    #[test]
    fn report_cursor_round_trip_keeps_microseconds_and_generated_ids() {
        let id = Uuid::new_v4();
        let encoded =
            encode_cursor("2030-01-01T00:00:00.123456Z", id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
        assert_eq!(
            decoded.at,
            Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                .single()
                .unwrap_or_else(|| unreachable!())
                + chrono::TimeDelta::microseconds(123_456)
        );
    }
}
