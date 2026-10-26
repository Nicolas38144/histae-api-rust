use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::shared::text::javascript_trim;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataRequestType {
    Access,
    Erasure,
    Portability,
    Rectification,
    Restriction,
    Objection,
}

impl DataRequestType {
    pub const fn requires_details(self) -> bool {
        matches!(self, Self::Rectification)
    }

    pub const fn is_retired(self) -> bool {
        matches!(self, Self::Restriction | Self::Objection)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Erasure => "erasure",
            Self::Portability => "portability",
            Self::Rectification => "rectification",
            Self::Restriction => "restriction",
            Self::Objection => "objection",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "access" => Some(Self::Access),
            "erasure" => Some(Self::Erasure),
            "portability" => Some(Self::Portability),
            "rectification" => Some(Self::Rectification),
            "restriction" => Some(Self::Restriction),
            "objection" => Some(Self::Objection),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRequestStatus {
    Pending,
    InProgress,
    Completed,
    Rejected,
}

impl DataRequestStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DataRequestTransition {
    InProgress,
    Completed,
    Rejected,
}

impl DataRequestTransition {
    pub const fn as_status(self) -> DataRequestStatus {
        match self {
            Self::InProgress => DataRequestStatus::InProgress,
            Self::Completed => DataRequestStatus::Completed,
            Self::Rejected => DataRequestStatus::Rejected,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DataRequestRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub request_type: DataRequestType,
    pub details: Option<String>,
    pub response: Option<String>,
    pub status: DataRequestStatus,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub handled_by: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DataRequest {
    pub id: Uuid,
    pub user_id: Uuid,
    #[serde(rename = "type")]
    pub request_type: DataRequestType,
    pub details: Option<String>,
    pub response: Option<String>,
    pub status: DataRequestStatus,
    pub requested_at: String,
    pub completed_at: Option<String>,
    pub handled_by: Option<Uuid>,
}

impl From<DataRequestRow> for DataRequest {
    fn from(row: DataRequestRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            request_type: row.request_type,
            details: row.details,
            response: row.response,
            status: row.status,
            requested_at: wire_timestamp(row.requested_at),
            completed_at: row.completed_at.map(wire_timestamp),
            handled_by: row.handled_by,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ErasureProgress {
    pub step: String,
    pub updated_at: String,
    pub event_id: Option<Uuid>,
    pub status: Option<String>,
    pub attempts: u16,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AdminDataRequestRow {
    pub request: DataRequestRow,
    pub notes: Option<String>,
    pub erasure: Option<ErasureProgress>,
    pub cursor_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdminDataRequest {
    #[serde(flatten)]
    pub request: DataRequest,
    pub notes: Option<String>,
    pub erasure: Option<ErasureProgress>,
}

impl From<AdminDataRequestRow> for AdminDataRequest {
    fn from(row: AdminDataRequestRow) -> Self {
        Self {
            request: row.request.into(),
            notes: row.notes,
            erasure: row.erasure,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DataAccessLogRow {
    pub id: Uuid,
    pub accessed_user_id: Uuid,
    pub accessor_id: Option<Uuid>,
    pub accessor_role: Option<String>,
    pub action: String,
    pub reason: Option<String>,
    pub accessed_at: DateTime<Utc>,
    pub cursor_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DataAccessLog {
    pub id: Uuid,
    pub accessed_user_id: Uuid,
    pub accessor_id: Option<Uuid>,
    pub accessor_role: Option<String>,
    pub action: String,
    pub reason: Option<String>,
    pub accessed_at: String,
}

impl From<DataAccessLogRow> for DataAccessLog {
    fn from(row: DataAccessLogRow) -> Self {
        Self {
            id: row.id,
            accessed_user_id: row.accessed_user_id,
            accessor_id: row.accessor_id,
            accessor_role: row.accessor_role,
            action: row.action,
            reason: row.reason,
            accessed_at: wire_timestamp(row.accessed_at),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateRequestResult {
    Updated,
    ErasureScheduled,
    NotFound,
    InvalidTransition,
    InvalidRequest,
}

#[derive(Clone, Debug)]
pub struct UpdateRequestInput {
    pub request_id: Uuid,
    pub status: DataRequestTransition,
    pub admin_id: Uuid,
    pub admin_role: AdminRole,
    pub notes: Option<String>,
    pub response: Option<String>,
}

pub type DataRightsFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait DataRightsStore: Send + Sync {
    fn create_request(
        &self,
        user_id: Uuid,
        request_type: DataRequestType,
        details: Option<String>,
    ) -> DataRightsFuture<'_, Option<DataRequestRow>>;

    fn requests_for_user(&self, user_id: Uuid) -> DataRightsFuture<'_, Vec<DataRequestRow>>;

    fn requests_for_admin(
        &self,
        status: Option<DataRequestStatus>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<AdminDataRequestRow>>;

    fn update_request(
        &self,
        input: UpdateRequestInput,
    ) -> DataRightsFuture<'_, UpdateRequestResult>;

    fn access_logs(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> DataRightsFuture<'_, Vec<DataAccessLogRow>>;

    fn record_self_export(&self, user_id: Uuid) -> DataRightsFuture<'_, ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRightsError {
    AlreadyOpen,
    InvalidRequest,
    InvalidCursor,
    InvalidPagination,
    RequestNotFound,
    InvalidTransition,
    Database(DatabaseError),
}

impl From<DatabaseError> for DataRightsError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone)]
pub struct DataRightsService {
    store: Arc<dyn DataRightsStore>,
}

impl DataRightsService {
    pub fn new(store: Arc<dyn DataRightsStore>) -> Self {
        Self { store }
    }

    pub async fn create_request(
        &self,
        user_id: Uuid,
        request_type: DataRequestType,
        details: Option<String>,
    ) -> Result<DataRequest, DataRightsError> {
        if !valid_request_details(request_type, details.as_deref()) {
            return Err(DataRightsError::InvalidRequest);
        }
        let details = details
            .map(|value| javascript_trim(&value).to_owned())
            .filter(|value| !value.is_empty());
        self.store
            .create_request(user_id, request_type, details)
            .await?
            .map(DataRequest::from)
            .ok_or(DataRightsError::AlreadyOpen)
    }

    pub async fn requests_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DataRequest>, DataRightsError> {
        self.store
            .requests_for_user(user_id)
            .await
            .map(|rows| rows.into_iter().map(DataRequest::from).collect())
            .map_err(DataRightsError::from)
    }

    pub async fn requests_for_admin(
        &self,
        status: Option<DataRequestStatus>,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<Page<AdminDataRequest>, DataRightsError> {
        validate_pagination(limit, offset, raw_cursor)?;
        let rows = self
            .store
            .requests_for_admin(status, limit + 1, offset, decode_cursor(raw_cursor)?)
            .await?;
        paginate(rows, limit, |row| (&row.cursor_at, row.request.id))
    }

    pub async fn update_request(
        &self,
        mut input: UpdateRequestInput,
    ) -> Result<UpdateRequestResult, DataRightsError> {
        if !valid_request_text(input.response.as_deref()) {
            return Err(DataRightsError::InvalidRequest);
        }
        input.response = input
            .response
            .map(|value| javascript_trim(&value).to_owned())
            .filter(|value| !value.is_empty());
        match self.store.update_request(input).await? {
            UpdateRequestResult::NotFound => Err(DataRightsError::RequestNotFound),
            UpdateRequestResult::InvalidTransition => Err(DataRightsError::InvalidTransition),
            UpdateRequestResult::InvalidRequest => Err(DataRightsError::InvalidRequest),
            result => Ok(result),
        }
    }

    pub async fn access_logs(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<Page<DataAccessLog>, DataRightsError> {
        validate_pagination(limit, offset, raw_cursor)?;
        let rows = self
            .store
            .access_logs(user_id, limit + 1, offset, decode_cursor(raw_cursor)?)
            .await?;
        paginate(rows, limit, |row| (&row.cursor_at, row.id))
    }
}

pub const REQUEST_DETAILS_MAX_LENGTH: usize = 2_000;

/// Legacy self-service request types may omit details. Manual requests need a
/// bounded explanation; never put this user-supplied content in operational logs.
pub fn valid_request_details(request_type: DataRequestType, details: Option<&str>) -> bool {
    let nonempty = details.is_some_and(|value| !javascript_trim(value).is_empty());
    !request_type.is_retired()
        && (!request_type.requires_details() || nonempty)
        && valid_request_text(details)
}

pub fn valid_request_text(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        let value = javascript_trim(value);
        value.chars().count() <= REQUEST_DETAILS_MAX_LENGTH
            && !value.chars().any(|character| {
                character.is_control() && !matches!(character, '\n' | '\r' | '\t')
            })
    })
}

fn paginate<Row, Public, Cursor>(
    rows: Vec<Row>,
    limit: u32,
    cursor: Cursor,
) -> Result<Page<Public>, DataRightsError>
where
    Public: From<Row>,
    Cursor: Fn(&Row) -> (&str, Uuid),
{
    let has_more = rows.len() > limit as usize;
    let next_cursor = if has_more {
        rows.get(limit as usize - 1)
            .map(|row| {
                let (at, id) = cursor(row);
                encode_cursor(at, id)
            })
            .transpose()?
    } else {
        None
    };
    Ok(Page {
        items: rows
            .into_iter()
            .take(limit as usize)
            .map(Public::from)
            .collect(),
        next_cursor,
    })
}

fn validate_pagination(
    limit: u32,
    offset: u32,
    cursor: Option<&str>,
) -> Result<(), DataRightsError> {
    if !(1..=100).contains(&limit) || (cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
    {
        return Err(DataRightsError::InvalidPagination);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, DataRightsError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| DataRightsError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&decoded).map_err(|_| DataRightsError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at) {
        return Err(DataRightsError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| DataRightsError::InvalidCursor)?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(&cursor.id).map_err(|_| DataRightsError::InvalidCursor)?;
    if !(1..=8).contains(&id.get_version_num()) || id.get_variant() != Variant::RFC4122 {
        return Err(DataRightsError::InvalidCursor);
    }
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, DataRightsError> {
    let payload = CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    };
    serde_json::to_vec(&payload)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| DataRightsError::InvalidCursor)
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

fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Store {
        rows: Mutex<Vec<DataRequestRow>>,
        update: Mutex<Option<UpdateRequestResult>>,
    }

    impl DataRightsStore for Store {
        fn create_request(
            &self,
            user_id: Uuid,
            request_type: DataRequestType,
            details: Option<String>,
        ) -> DataRightsFuture<'_, Option<DataRequestRow>> {
            Box::pin(async move {
                let mut rows = self.rows.lock().map_err(|_| DatabaseError::QueryFailed)?;
                if rows.iter().any(|row| row.request_type == request_type) {
                    return Ok(None);
                }
                let mut row = data_request(user_id, request_type);
                row.details = details;
                rows.push(row.clone());
                Ok(Some(row))
            })
        }

        fn requests_for_user(&self, _user_id: Uuid) -> DataRightsFuture<'_, Vec<DataRequestRow>> {
            Box::pin(async move {
                self.rows
                    .lock()
                    .map(|rows| rows.clone())
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn requests_for_admin(
            &self,
            _status: Option<DataRequestStatus>,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> DataRightsFuture<'_, Vec<AdminDataRequestRow>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn update_request(
            &self,
            _input: UpdateRequestInput,
        ) -> DataRightsFuture<'_, UpdateRequestResult> {
            Box::pin(async move {
                self.update
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .ok_or(DatabaseError::QueryFailed)
            })
        }

        fn access_logs(
            &self,
            _user_id: Uuid,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> DataRightsFuture<'_, Vec<DataAccessLogRow>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn record_self_export(&self, _user_id: Uuid) -> DataRightsFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn data_request(user_id: Uuid, request_type: DataRequestType) -> DataRequestRow {
        DataRequestRow {
            id: Uuid::new_v4(),
            user_id,
            request_type,
            details: None,
            response: None,
            status: DataRequestStatus::Pending,
            requested_at: Utc::now(),
            completed_at: None,
            handled_by: None,
        }
    }

    #[tokio::test]
    async fn preserves_open_request_conflicts_and_public_shape() {
        let store = Arc::new(Store::default());
        let service = DataRightsService::new(store);
        let user_id = Uuid::new_v4();
        let created = service
            .create_request(user_id, DataRequestType::Access, None)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(created.user_id, user_id);
        assert_eq!(created.request_type, DataRequestType::Access);
        assert_eq!(
            service
                .create_request(user_id, DataRequestType::Access, None)
                .await,
            Err(DataRightsError::AlreadyOpen)
        );
    }

    #[tokio::test]
    async fn maps_missing_and_invalid_transitions_without_losing_idempotent_scheduling() {
        let store = Arc::new(Store::default());
        let service = DataRightsService::new(store.clone());
        let input = || UpdateRequestInput {
            request_id: Uuid::new_v4(),
            status: DataRequestTransition::Completed,
            admin_id: Uuid::new_v4(),
            admin_role: AdminRole::Admin,
            notes: None,
            response: None,
        };

        if let Ok(mut result) = store.update.lock() {
            *result = Some(UpdateRequestResult::NotFound);
        }
        assert_eq!(
            service.update_request(input()).await,
            Err(DataRightsError::RequestNotFound)
        );
        if let Ok(mut result) = store.update.lock() {
            *result = Some(UpdateRequestResult::InvalidTransition);
        }
        assert_eq!(
            service.update_request(input()).await,
            Err(DataRightsError::InvalidTransition)
        );
        if let Ok(mut result) = store.update.lock() {
            *result = Some(UpdateRequestResult::ErasureScheduled);
        }
        assert_eq!(
            service.update_request(input()).await,
            Ok(UpdateRequestResult::ErasureScheduled)
        );
    }

    #[tokio::test]
    async fn correction_details_are_bounded_and_retired_requests_are_rejected() {
        let service = DataRightsService::new(Arc::new(Store::default()));
        let user = Uuid::new_v4();
        for request_type in [
            DataRequestType::Rectification,
            DataRequestType::Restriction,
            DataRequestType::Objection,
        ] {
            for details in [
                None,
                Some(" \n\t".to_owned()),
                Some("a".repeat(REQUEST_DETAILS_MAX_LENGTH + 1)),
                Some("bad\0value".to_owned()),
            ] {
                assert_eq!(
                    service.create_request(user, request_type, details).await,
                    Err(DataRightsError::InvalidRequest)
                );
            }
        }
        for retired in [DataRequestType::Restriction, DataRequestType::Objection] {
            assert_eq!(
                service.create_request(user, retired, Some("A valid message".to_owned())).await,
                Err(DataRightsError::InvalidRequest)
            );
        }
        let message = "Please correct this historical information.\nI cannot edit it in the app.";
        let created = service
            .create_request(user, DataRequestType::Rectification, Some(format!("  {message}  ")))
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(created.details.as_deref(), Some(message));
        let history = service.requests_for_user(user).await.unwrap_or_else(|_| unreachable!());
        assert_eq!(history[0].details, created.details);
    }

    #[test]
    fn cursor_round_trip_preserves_microseconds_and_generated_ids() {
        let id = Uuid::new_v4();
        let encoded =
            encode_cursor("2030-01-01T00:00:00.123456Z", id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.at.timestamp_subsec_micros(), 123_456);
        assert_eq!(
            decode_cursor(Some("not-a-cursor")),
            Err(DataRightsError::InvalidCursor)
        );
    }
}
