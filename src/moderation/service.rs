use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;
use uuid::{Uuid, Variant};

use super::domain::{
    ModerationCase, ModerationContentType, ModerationDecision, ModerationDetail,
    ModerationReviewInput, ModerationReviewResult, PageCursor, PhotoReviewChecks,
};
use super::store::ModerationStore;
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::media::service::PhotoService;
use crate::profiles::domain::ModerationStatus;
use crate::shared::text::{javascript_trim, validator_js_length};

pub type PhotoUrlFuture<'a> = Pin<Box<dyn Future<Output = Result<Option<String>, ()>> + Send + 'a>>;

pub trait ModerationPhotoUrlProvider: Send + Sync {
    fn url_for_key(&self, object_key: Option<String>) -> PhotoUrlFuture<'_>;
}

impl ModerationPhotoUrlProvider for PhotoService {
    fn url_for_key(&self, object_key: Option<String>) -> PhotoUrlFuture<'_> {
        Box::pin(async move { self.url_for_object_key(object_key).await.map_err(|_| ()) })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModerationError {
    InvalidRequest,
    InvalidCursor,
    NotFound,
    Stale,
    ReviewNotAllowed,
    PhotoStorageUnavailable,
    Database(DatabaseError),
}

impl From<DatabaseError> for ModerationError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct ModerationService {
    store: Arc<dyn ModerationStore>,
    photos: Arc<dyn ModerationPhotoUrlProvider>,
}

impl ModerationService {
    pub fn new(
        store: Arc<dyn ModerationStore>,
        photos: Arc<dyn ModerationPhotoUrlProvider>,
    ) -> Self {
        Self { store, photos }
    }

    pub async fn list(
        &self,
        status: Option<ModerationStatus>,
        content_type: Option<ModerationContentType>,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<Page<ModerationCase>, ModerationError> {
        if !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(ModerationError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let rows = self
            .store
            .list(status, content_type, limit + 1, offset, cursor)
            .await?;
        let has_more = rows.len() > limit as usize;
        let items = rows
            .iter()
            .take(limit as usize)
            .map(|row| row.to_case())
            .collect();
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.id))
                .transpose()?
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn detail(
        &self,
        case_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        raw_reason: &str,
    ) -> Result<ModerationDetail, ModerationError> {
        let reason =
            normalize_moderation_reason(admin_role.audit_reason(raw_reason, "Superadmin access"))?;
        let row = self
            .store
            .detail(case_id, admin_id, admin_role, reason)
            .await?
            .ok_or(ModerationError::NotFound)?;
        let photo = self
            .photos
            .url_for_key(row.object_key.clone())
            .await
            .map_err(|_| ModerationError::PhotoStorageUnavailable)?;
        Ok(ModerationDetail {
            case: row.to_case(),
            content: row.text_content,
            question: row.question,
            photo,
        })
    }

    pub async fn review(
        &self,
        case_id: Uuid,
        mut input: ModerationReviewInput,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> Result<(), ModerationError> {
        if input.version < 1 || !coherent_photo_checks(input.decision, input.photo_checks) {
            return Err(ModerationError::InvalidRequest);
        }
        input.reason = normalize_moderation_reason(
            admin_role.audit_reason(&input.reason, "Superadmin moderation"),
        )?;
        match self
            .store
            .review(case_id, input, admin_id, admin_role)
            .await?
        {
            ModerationReviewResult::Updated => Ok(()),
            ModerationReviewResult::NotFound => Err(ModerationError::NotFound),
            ModerationReviewResult::Stale => Err(ModerationError::Stale),
            ModerationReviewResult::NotActionable => Err(ModerationError::ReviewNotAllowed),
        }
    }
}

pub fn coherent_photo_checks(
    decision: ModerationDecision,
    checks: Option<PhotoReviewChecks>,
) -> bool {
    let Some(checks) = checks else {
        return true;
    };
    let all = checks.face_detectable && checks.sharp_enough && checks.content_allowed;
    match decision {
        ModerationDecision::Approved => all,
        ModerationDecision::Rejected => !all,
    }
}

pub fn normalize_moderation_reason(value: &str) -> Result<String, ModerationError> {
    let normalized = value.nfkc().collect::<String>();
    let reason = javascript_trim(&normalized).to_owned();
    if !(3..=500).contains(&validator_js_length(&reason)) || reason.len() > 1_000 {
        return Err(ModerationError::InvalidRequest);
    }
    Ok(reason)
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, ModerationError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.encode_utf16().count() > 512 {
        return Err(ModerationError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ModerationError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| ModerationError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| ModerationError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || !canonical_uuid(&cursor.id, id)
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != Variant::RFC4122
    {
        return Err(ModerationError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| ModerationError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, ModerationError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| ModerationError::InvalidCursor)
}

fn canonical_uuid(value: &str, parsed: Uuid) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
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

    use super::*;
    use crate::moderation::domain::{ModerationRow, wire_timestamp};
    use crate::moderation::store::{ModerationStore, ModerationStoreFuture};
    use crate::profiles::domain::ModerationReason;

    #[test]
    fn enforces_the_three_photo_checks() {
        let all_true = PhotoReviewChecks {
            face_detectable: true,
            sharp_enough: true,
            content_allowed: true,
        };
        let one_false = PhotoReviewChecks {
            sharp_enough: false,
            ..all_true
        };
        assert!(coherent_photo_checks(
            ModerationDecision::Approved,
            Some(all_true)
        ));
        assert!(!coherent_photo_checks(
            ModerationDecision::Approved,
            Some(one_false)
        ));
        assert!(!coherent_photo_checks(
            ModerationDecision::Rejected,
            Some(all_true)
        ));
        assert!(coherent_photo_checks(
            ModerationDecision::Rejected,
            Some(one_false)
        ));
    }

    #[test]
    fn normalizes_and_bounds_audit_reasons() {
        assert_eq!(
            normalize_moderation_reason("  V\u{0061}\u{0301}lidation  "),
            Ok("Válidation".to_owned())
        );
        assert_eq!(
            normalize_moderation_reason(" x "),
            Err(ModerationError::InvalidRequest)
        );
        assert_eq!(
            normalize_moderation_reason(&"é".repeat(501)),
            Err(ModerationError::InvalidRequest)
        );
    }

    #[test]
    fn cursor_round_trip_preserves_microseconds_and_generated_uuids() {
        let id = Uuid::new_v4();
        let at = "2030-01-01T00:00:00.123456Z";
        let encoded = encode_cursor(at, id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.at.to_rfc3339(), "2030-01-01T00:00:00.123456+00:00");
    }

    #[derive(Clone)]
    struct OrderedStore {
        events: Arc<Mutex<Vec<&'static str>>>,
        row: Option<ModerationRow>,
    }

    impl ModerationStore for OrderedStore {
        fn list<'a>(
            &'a self,
            _status: Option<ModerationStatus>,
            _content_type: Option<ModerationContentType>,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> ModerationStoreFuture<'a, Vec<ModerationRow>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn detail(
            &self,
            _case_id: Uuid,
            _admin_id: Uuid,
            _admin_role: AdminRole,
            _reason: String,
        ) -> ModerationStoreFuture<'_, Option<ModerationRow>> {
            Box::pin(async move {
                if self.row.is_some() {
                    self.events
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push("audit");
                }
                Ok(self.row.clone())
            })
        }

        fn review(
            &self,
            _case_id: Uuid,
            _input: ModerationReviewInput,
            _admin_id: Uuid,
            _admin_role: AdminRole,
        ) -> ModerationStoreFuture<'_, ModerationReviewResult> {
            Box::pin(async { Ok(ModerationReviewResult::NotFound) })
        }
    }

    struct OrderedPhotos {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ModerationPhotoUrlProvider for OrderedPhotos {
        fn url_for_key(&self, _object_key: Option<String>) -> PhotoUrlFuture<'_> {
            Box::pin(async move {
                self.events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push("sign");
                Ok(Some("https://storage.test/signed".to_owned()))
            })
        }
    }

    fn photo_row(case_id: Uuid, user_id: Uuid) -> ModerationRow {
        let now = Utc::now();
        ModerationRow {
            id: case_id,
            user_id,
            firstname: Some("Alice".to_owned()),
            content_type: ModerationContentType::Photo,
            status: ModerationStatus::Pending,
            reason_codes: vec![ModerationReason::Blurry],
            policy_version: "local_vision_v1".to_owned(),
            version: 1,
            face_count: Some(1),
            sharpness_score: Some(40.0),
            nsfw_score: Some(0.1),
            face_detectable: None,
            sharp_enough: None,
            content_allowed: None,
            review_reason: None,
            reviewed_at: None,
            reviewed_by: None,
            created_at: now,
            updated_at: now,
            cursor_at: wire_timestamp(now),
            text_content: None,
            question: None,
            object_key: Some(format!("profile-photos/{user_id}/{case_id}.webp")),
        }
    }

    #[tokio::test]
    async fn commits_the_audited_detail_before_signing_a_photo() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let case_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let service = ModerationService::new(
            Arc::new(OrderedStore {
                events: Arc::clone(&events),
                row: Some(photo_row(case_id, user_id)),
            }),
            Arc::new(OrderedPhotos {
                events: Arc::clone(&events),
            }),
        );
        let detail = service
            .detail(case_id, Uuid::new_v4(), AdminRole::Admin, "Contrôle manuel")
            .await
            .unwrap_or_else(|error| panic!("detail succeeds: {error:?}"));
        assert_eq!(detail.photo.as_deref(), Some("https://storage.test/signed"));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            vec!["audit", "sign"]
        );
    }

    #[tokio::test]
    async fn missing_cases_do_not_attempt_photo_signing() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let service = ModerationService::new(
            Arc::new(OrderedStore {
                events: Arc::clone(&events),
                row: None,
            }),
            Arc::new(OrderedPhotos {
                events: Arc::clone(&events),
            }),
        );
        let result = service
            .detail(
                Uuid::new_v4(),
                Uuid::new_v4(),
                AdminRole::Admin,
                "Contrôle manuel",
            )
            .await;
        assert!(matches!(result, Err(ModerationError::NotFound)));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            Vec::<&'static str>::new()
        );
    }
}
