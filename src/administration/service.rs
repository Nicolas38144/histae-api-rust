use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{
    AdminUser, AdminUserDetail, AdminUserRole, AdminUserStatus, BanResult, PageCursor,
    RoleChangeResult,
};
use super::store::AdministrationStore;
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::matches::domain::{PublicMatch, PublicMessage};
use crate::profiles::service::ProfilePhotoUrlProvider;
use crate::shared::text::{javascript_trim, validator_js_length};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdministrationError {
    InvalidRequest,
    InvalidCursor,
    AccountNotFound,
    MatchNotFound,
    ActionForbidden,
    PhotoStorageUnavailable,
    Database(DatabaseError),
}

impl From<DatabaseError> for AdministrationError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

pub struct AuditedPageRequest<'a> {
    pub resource_id: Uuid,
    pub admin_id: Uuid,
    pub admin_role: AdminRole,
    pub reason: &'a str,
    pub limit: u32,
    pub offset: u32,
    pub cursor: Option<&'a str>,
}

#[derive(Clone)]
pub struct AdministrationService {
    store: Arc<dyn AdministrationStore>,
    photos: Arc<dyn ProfilePhotoUrlProvider>,
    terms_version: Arc<str>,
    privacy_version: Arc<str>,
}

impl AdministrationService {
    pub fn new(
        store: Arc<dyn AdministrationStore>,
        photos: Arc<dyn ProfilePhotoUrlProvider>,
        terms_version: String,
        privacy_version: String,
    ) -> Self {
        Self {
            store,
            photos,
            terms_version: terms_version.into(),
            privacy_version: privacy_version.into(),
        }
    }

    pub async fn users(
        &self,
        status: Option<AdminUserStatus>,
        role: Option<AdminUserRole>,
        raw_search: Option<&str>,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<Page<AdminUser>, AdministrationError> {
        validate_pagination(limit, offset, raw_cursor)?;
        let cursor = decode_cursor(raw_cursor)?;
        let search = raw_search.map(javascript_trim).unwrap_or_default();
        let rows = self
            .store
            .list_users(
                status,
                role,
                search.to_owned(),
                limit + 1,
                offset,
                cursor,
                self.terms_version.to_string(),
                self.privacy_version.to_string(),
            )
            .await?;
        let has_more = rows.len() > limit as usize;
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.id))
                .transpose()?
        } else {
            None
        };
        Ok(Page {
            items: rows
                .into_iter()
                .take(limit as usize)
                .map(|row| row.into_public(None))
                .collect(),
            next_cursor,
        })
    }

    pub async fn user_detail(
        &self,
        target_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        raw_reason: &str,
    ) -> Result<AdminUserDetail, AdministrationError> {
        let reason = normalize_reason(raw_reason)?;
        let row = self
            .store
            .user_detail(
                target_id,
                admin_id,
                admin_role,
                reason,
                self.terms_version.to_string(),
                self.privacy_version.to_string(),
            )
            .await?
            .ok_or(AdministrationError::AccountNotFound)?;
        let photo = self
            .photos
            .url_for_key(row.user.photo_object_key.clone())
            .await
            .map_err(|_| AdministrationError::PhotoStorageUnavailable)?;
        Ok(AdminUserDetail {
            user: row.user.into_public(photo),
            banned_reason: row.banned_reason,
            preferences: row.preferences,
            traits: row.traits,
            consents: row.consents,
            presence: row.presence,
        })
    }

    pub async fn update_ban(
        &self,
        target_id: Uuid,
        is_banned: bool,
        raw_reason: Option<&str>,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> Result<(), AdministrationError> {
        let reason = if is_banned {
            normalize_reason(raw_reason.unwrap_or_default())?
        } else {
            normalize_reason(
                raw_reason
                    .filter(|value| !value.is_empty())
                    .unwrap_or("Administrative unban"),
            )?
        };
        match self
            .store
            .set_ban(target_id, is_banned, reason, admin_id, admin_role)
            .await?
        {
            BanResult::Updated => Ok(()),
            BanResult::NotFound => Err(AdministrationError::AccountNotFound),
            BanResult::Forbidden => Err(AdministrationError::ActionForbidden),
        }
    }

    pub async fn update_role(
        &self,
        target_id: Uuid,
        role: AdminUserRole,
        raw_reason: &str,
        actor_id: Uuid,
        actor_role: AdminRole,
    ) -> Result<(), AdministrationError> {
        if actor_role != AdminRole::Superadmin
            || target_id == actor_id
            || role == AdminUserRole::Superadmin
        {
            return Err(AdministrationError::ActionForbidden);
        }
        let reason = normalize_reason(raw_reason)?;
        match self
            .store
            .set_role(target_id, role, reason, actor_id)
            .await?
        {
            RoleChangeResult::Updated | RoleChangeResult::Unchanged => Ok(()),
            RoleChangeResult::NotFound => Err(AdministrationError::AccountNotFound),
            RoleChangeResult::Forbidden => Err(AdministrationError::ActionForbidden),
        }
    }

    pub async fn matches(
        &self,
        request: AuditedPageRequest<'_>,
    ) -> Result<Page<PublicMatch>, AdministrationError> {
        validate_pagination(request.limit, request.offset, request.cursor)?;
        let reason = normalize_reason(request.reason)?;
        let rows = self
            .store
            .matches(
                request.resource_id,
                request.admin_id,
                request.admin_role,
                reason,
                request.limit + 1,
                request.offset,
                decode_cursor(request.cursor)?,
            )
            .await?
            .ok_or(AdministrationError::AccountNotFound)?;
        let has_more = rows.len() > request.limit as usize;
        let next_cursor = if has_more {
            rows.get(request.limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.item.id))
                .transpose()?
        } else {
            None
        };
        Ok(Page {
            items: rows
                .into_iter()
                .take(request.limit as usize)
                .map(|row| PublicMatch::from(row.item))
                .collect(),
            next_cursor,
        })
    }

    pub async fn messages(
        &self,
        request: AuditedPageRequest<'_>,
    ) -> Result<Page<PublicMessage>, AdministrationError> {
        validate_pagination(request.limit, request.offset, request.cursor)?;
        let reason = normalize_reason(request.reason)?;
        let rows = self
            .store
            .messages(
                request.resource_id,
                request.admin_id,
                request.admin_role,
                reason,
                request.limit + 1,
                request.offset,
                decode_cursor(request.cursor)?,
            )
            .await?
            .ok_or(AdministrationError::MatchNotFound)?;
        let has_more = rows.len() > request.limit as usize;
        let next_cursor = if has_more {
            rows.get(request.limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.item.id))
                .transpose()?
        } else {
            None
        };
        Ok(Page {
            items: rows
                .into_iter()
                .take(request.limit as usize)
                .map(|row| PublicMessage::from(row.item))
                .collect(),
            next_cursor,
        })
    }
}

fn normalize_reason(value: &str) -> Result<String, AdministrationError> {
    let reason = javascript_trim(value);
    if !(3..=500).contains(&reason.encode_utf16().count()) {
        return Err(AdministrationError::InvalidRequest);
    }
    Ok(reason.to_owned())
}

fn validate_pagination(
    limit: u32,
    offset: u32,
    cursor: Option<&str>,
) -> Result<(), AdministrationError> {
    if !(1..=100).contains(&limit) || (cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
    {
        return Err(AdministrationError::InvalidRequest);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, AdministrationError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if validator_js_length(value) > 512 {
        return Err(AdministrationError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AdministrationError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| AdministrationError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| AdministrationError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || cursor.id.len() != 36
        || id.hyphenated().to_string() != cursor.id.to_ascii_lowercase()
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != Variant::RFC4122
    {
        return Err(AdministrationError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| AdministrationError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, AdministrationError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| AdministrationError::InvalidCursor)
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

    use chrono::{NaiveDate, TimeZone as _};

    use super::*;
    use crate::administration::domain::{
        AdminUserDetailRow, AdminUserRow, BanResult, CursorMatchRow, CursorMessageRow,
    };
    use crate::administration::store::AdministrationStoreFuture;
    use crate::profiles::service::ProfilePhotoUrlFuture;

    struct FakeStore {
        user: AdminUserRow,
        ban_result: BanResult,
        reasons: Mutex<Vec<String>>,
    }

    #[allow(clippy::too_many_arguments)]
    impl AdministrationStore for FakeStore {
        fn list_users(
            &self,
            _status: Option<AdminUserStatus>,
            _role: Option<AdminUserRole>,
            _search: String,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
            _terms_version: String,
            _privacy_version: String,
        ) -> AdministrationStoreFuture<'_, Vec<AdminUserRow>> {
            Box::pin(async move { Ok(vec![self.user.clone()]) })
        }

        fn user_detail(
            &self,
            _target_id: Uuid,
            _admin_id: Uuid,
            _admin_role: AdminRole,
            reason: String,
            _terms_version: String,
            _privacy_version: String,
        ) -> AdministrationStoreFuture<'_, Option<AdminUserDetailRow>> {
            Box::pin(async move {
                self.reasons
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(reason);
                Ok(Some(AdminUserDetailRow {
                    user: self.user.clone(),
                    banned_reason: None,
                    preferences: None,
                    traits: Vec::new(),
                    consents: Vec::new(),
                    presence: None,
                }))
            })
        }

        fn set_ban(
            &self,
            _target_id: Uuid,
            _is_banned: bool,
            reason: String,
            _admin_id: Uuid,
            _admin_role: AdminRole,
        ) -> AdministrationStoreFuture<'_, BanResult> {
            Box::pin(async move {
                self.reasons
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(reason);
                Ok(self.ban_result)
            })
        }

        fn set_role(
            &self,
            _target_id: Uuid,
            _role: AdminUserRole,
            reason: String,
            _actor_id: Uuid,
        ) -> AdministrationStoreFuture<'_, RoleChangeResult> {
            Box::pin(async move {
                self.reasons
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(reason);
                Ok(RoleChangeResult::Updated)
            })
        }

        fn matches(
            &self,
            _user_id: Uuid,
            _admin_id: Uuid,
            _admin_role: AdminRole,
            _reason: String,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMatchRow>>> {
            Box::pin(async { Ok(Some(Vec::new())) })
        }

        fn messages(
            &self,
            _match_id: Uuid,
            _admin_id: Uuid,
            _admin_role: AdminRole,
            _reason: String,
            _limit: u32,
            _offset: u32,
            _cursor: Option<PageCursor>,
        ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMessageRow>>> {
            Box::pin(async { Ok(Some(Vec::new())) })
        }
    }

    #[derive(Default)]
    struct Photos(Mutex<Vec<Option<String>>>);

    impl ProfilePhotoUrlProvider for Photos {
        fn url_for_key(&self, key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
            Box::pin(async move {
                self.0.lock().map_err(|_| ())?.push(key.clone());
                Ok(key.map(|value| format!("https://signed.invalid/{value}")))
            })
        }
    }

    fn user() -> AdminUserRow {
        AdminUserRow {
            id: Uuid::new_v4(),
            role: AdminUserRole::User,
            is_banned: false,
            banned_at: None,
            created_at: Utc
                .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                .single()
                .unwrap_or_else(|| unreachable!()),
            firstname: Some("Alice".to_owned()),
            birthdate: NaiveDate::from_ymd_opt(1990, 1, 2),
            sex: None,
            photo_object_key: Some(format!(
                "profile-photos/{}/{}.webp",
                Uuid::new_v4(),
                Uuid::new_v4()
            )),
            plan: "free".to_owned(),
            onboarding_complete: true,
            reports_received: 1,
            matches_count: 2,
            cursor_at: "2030-01-01T00:00:00.000000Z".to_owned(),
        }
    }

    fn service(ban_result: BanResult) -> (AdministrationService, Arc<FakeStore>, Arc<Photos>) {
        let store = Arc::new(FakeStore {
            user: user(),
            ban_result,
            reasons: Mutex::new(Vec::new()),
        });
        let photos = Arc::new(Photos::default());
        (
            AdministrationService::new(
                store.clone(),
                photos.clone(),
                "terms-v1".to_owned(),
                "privacy-v1".to_owned(),
            ),
            store,
            photos,
        )
    }

    #[tokio::test]
    async fn admin_lists_never_sign_photos_but_audited_details_do() {
        let (service, store, photos) = service(BanResult::Updated);
        let page = service
            .users(None, None, None, 20, 0, None)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].photo, None);
        assert!(photos.0.lock().is_ok_and(|calls| calls.is_empty()));

        let detail = service
            .user_detail(
                store.user.id,
                Uuid::new_v4(),
                AdminRole::Admin,
                "  Enquête sécurité  ",
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert!(detail.user.photo.is_some());
        assert!(
            store
                .reasons
                .lock()
                .is_ok_and(|items| items.as_slice() == ["Enquête sécurité"])
        );
        assert!(photos.0.lock().is_ok_and(|calls| calls.len() == 1));
    }

    #[tokio::test]
    async fn ban_permissions_and_default_unban_reason_are_stable() {
        let (forbidden, _, _) = service(BanResult::Forbidden);
        assert_eq!(
            forbidden
                .update_ban(
                    Uuid::new_v4(),
                    true,
                    Some("Incident sécurité"),
                    Uuid::new_v4(),
                    AdminRole::Admin,
                )
                .await,
            Err(AdministrationError::ActionForbidden)
        );

        let (allowed, store, _) = service(BanResult::Updated);
        assert_eq!(
            allowed
                .update_ban(
                    Uuid::new_v4(),
                    false,
                    None,
                    Uuid::new_v4(),
                    AdminRole::Superadmin,
                )
                .await,
            Ok(())
        );
        assert!(
            store
                .reasons
                .lock()
                .is_ok_and(|items| items.as_slice() == ["Administrative unban"])
        );
    }

    #[tokio::test]
    async fn only_superadmin_can_change_user_or_admin_roles() {
        let (service, store, _) = service(BanResult::Updated);
        let actor_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        assert_eq!(
            service
                .update_role(
                    target_id,
                    AdminUserRole::Admin,
                    "Mission",
                    actor_id,
                    AdminRole::Admin
                )
                .await,
            Err(AdministrationError::ActionForbidden)
        );
        assert_eq!(
            service
                .update_role(
                    target_id,
                    AdminUserRole::Superadmin,
                    "Mission",
                    actor_id,
                    AdminRole::Superadmin
                )
                .await,
            Err(AdministrationError::ActionForbidden)
        );
        assert_eq!(
            service
                .update_role(
                    actor_id,
                    AdminUserRole::User,
                    "Mission",
                    actor_id,
                    AdminRole::Superadmin
                )
                .await,
            Err(AdministrationError::ActionForbidden)
        );
        assert!(store.reasons.lock().is_ok_and(|items| items.is_empty()));
        assert_eq!(
            service
                .update_role(
                    target_id,
                    AdminUserRole::Admin,
                    "  Nouvelle mission  ",
                    actor_id,
                    AdminRole::Superadmin
                )
                .await,
            Ok(())
        );
        assert_eq!(
            service
                .update_role(
                    target_id,
                    AdminUserRole::User,
                    "Fin de mission",
                    actor_id,
                    AdminRole::Superadmin
                )
                .await,
            Ok(())
        );
        assert!(
            store
                .reasons
                .lock()
                .is_ok_and(|items| items.as_slice() == ["Nouvelle mission", "Fin de mission"])
        );
    }

    #[test]
    fn admin_cursor_round_trip_uses_generated_identifiers() {
        let id = Uuid::new_v4();
        let encoded =
            encode_cursor("2030-01-01T00:00:00.123456Z", id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
    }
}
