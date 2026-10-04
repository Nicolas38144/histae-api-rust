use super::{
    AdminPhotoError, AdminPhotoPage, AdminPhotoStore, OUTBOX_LOCK_STALE_MINUTES,
    PHOTO_PROCESSING_STALE_MINUTES, PhotoReconciliationFilter, ReconciliationResult,
};
use crate::identity::admin_role::AdminRole;
use crate::moderation::domain::PageCursor;
use crate::shared::{
    clock::Clock,
    text::{javascript_trim, validator_js_length},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Clone)]
pub struct AdminPhotoService {
    store: Arc<dyn AdminPhotoStore>,
    clock: Arc<dyn Clock>,
}

impl AdminPhotoService {
    pub fn new(store: Arc<dyn AdminPhotoStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub async fn list(
        &self,
        filter: PhotoReconciliationFilter,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<AdminPhotoPage, AdminPhotoError> {
        if !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(AdminPhotoError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let stale_before = self.clock.now() - TimeDelta::minutes(PHOTO_PROCESSING_STALE_MINUTES);
        let rows = self
            .store
            .list(filter, stale_before, limit + 1, offset, cursor)
            .await?;
        let has_more = rows.len() > limit as usize;
        let photos = rows
            .iter()
            .take(limit as usize)
            .map(|row| row.item.clone())
            .collect();
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.item.photo_id))
                .transpose()?
        } else {
            None
        };
        Ok(AdminPhotoPage {
            photos,
            next_cursor,
        })
    }

    pub async fn reconcile(
        &self,
        photo_id: Uuid,
        raw_reason: &str,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> Result<(), AdminPhotoError> {
        let reason = normalize_admin_reason(raw_reason)?;
        let now = self.clock.now();
        match self
            .store
            .reconcile(
                photo_id,
                now - TimeDelta::minutes(PHOTO_PROCESSING_STALE_MINUTES),
                now - TimeDelta::minutes(OUTBOX_LOCK_STALE_MINUTES),
                admin_id,
                admin_role,
                reason,
            )
            .await?
        {
            ReconciliationResult::Queued => Ok(()),
            ReconciliationResult::NotFound => Err(AdminPhotoError::NotFound),
            ReconciliationResult::NotActionable => Err(AdminPhotoError::NotActionable),
            ReconciliationResult::AlreadyProcessing => Err(AdminPhotoError::AlreadyProcessing),
        }
    }
}

fn normalize_admin_reason(value: &str) -> Result<String, AdminPhotoError> {
    let reason = javascript_trim(value).to_owned();
    if !(3..=500).contains(&validator_js_length(&reason)) {
        return Err(AdminPhotoError::InvalidRequest);
    }
    Ok(reason)
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, AdminPhotoError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.encode_utf16().count() > 512 {
        return Err(AdminPhotoError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AdminPhotoError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| AdminPhotoError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| AdminPhotoError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || !canonical_uuid(&cursor.id, id)
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(AdminPhotoError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| AdminPhotoError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, AdminPhotoError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| AdminPhotoError::InvalidCursor)
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

fn canonical_uuid(value: &str, parsed: Uuid) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_bounds_operator_reasons() {
        assert_eq!(
            normalize_admin_reason("  Incident confirmé  "),
            Ok("Incident confirmé".to_owned())
        );
        assert_eq!(
            normalize_admin_reason(" x "),
            Err(AdminPhotoError::InvalidRequest)
        );
    }
}
