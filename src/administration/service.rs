use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{
    AdminMatch, AdminUser, AdminUserDetail, AdminUserRole, AdminUserStatus, BanResult, PageCursor,
    PremiumGrantResult, RoleChangeResult,
};
use super::store::AdministrationStore;
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::matches::domain::{PublicMatch, PublicMessage};
use crate::profiles::service::ProfilePhotoUrlProvider;
use crate::shared::clock::Clock;
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
    clock: Arc<dyn Clock>,
}

impl AdministrationService {
    pub fn new(
        store: Arc<dyn AdministrationStore>,
        photos: Arc<dyn ProfilePhotoUrlProvider>,
        terms_version: String,
        privacy_version: String,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            photos,
            terms_version: terms_version.into(),
            privacy_version: privacy_version.into(),
            clock,
        }
    }

    pub async fn user_names(
        &self,
        ids: Vec<Uuid>,
    ) -> Result<Vec<(Uuid, Option<String>)>, AdministrationError> {
        self.store.user_names(ids).await.map_err(Into::into)
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
        let reason = normalize_reason(admin_role.audit_reason(raw_reason, "Superadmin access"))?;
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
            complimentary_premium: row.complimentary_premium,
            is_review_account: row.is_review_account,
            banned_reason: row.banned_reason,
            preferences: row.preferences,
            traits: row.traits,
            consents: row.consents,
            presence: row.presence,
            continuation_quota: row.continuation_quota,
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
            normalize_reason(
                admin_role.audit_reason(raw_reason.unwrap_or_default(), "Superadmin ban"),
            )?
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

    pub async fn update_complimentary_premium(
        &self,
        target_id: Uuid,
        enabled: bool,
        raw_reason: &str,
        actor_id: Uuid,
        actor_role: AdminRole,
    ) -> Result<(), AdministrationError> {
        if actor_role != AdminRole::Superadmin {
            return Err(AdministrationError::ActionForbidden);
        }
        let reason = normalize_reason(actor_role.audit_reason(
            raw_reason,
            if enabled {
                "Superadmin complimentary Premium grant"
            } else {
                "Superadmin complimentary Premium revocation"
            },
        ))?;
        match self
            .store
            .set_complimentary_premium(target_id, enabled, reason, actor_id)
            .await?
        {
            PremiumGrantResult::Updated | PremiumGrantResult::Unchanged => Ok(()),
            PremiumGrantResult::NotFound => Err(AdministrationError::AccountNotFound),
            PremiumGrantResult::Forbidden => Err(AdministrationError::ActionForbidden),
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
        let reason =
            normalize_reason(actor_role.audit_reason(raw_reason, "Superadmin role change"))?;
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
    ) -> Result<Page<AdminMatch>, AdministrationError> {
        validate_pagination(request.limit, request.offset, request.cursor)?;
        let reason = normalize_reason(
            request
                .admin_role
                .audit_reason(request.reason, "Superadmin access"),
        )?;
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
        let now = self.clock.now();
        Ok(Page {
            items: rows
                .into_iter()
                .take(request.limit as usize)
                .map(|row| AdminMatch {
                    item: PublicMatch::from_record_at(row.item, now),
                    can_view_messages: row.can_view_messages,
                })
                .collect(),
            next_cursor,
        })
    }

    pub async fn messages(
        &self,
        request: AuditedPageRequest<'_>,
    ) -> Result<Page<PublicMessage>, AdministrationError> {
        validate_pagination(request.limit, request.offset, request.cursor)?;
        let reason = normalize_reason(
            request
                .admin_role
                .audit_reason(request.reason, "Superadmin access"),
        )?;
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
        matches: Vec<CursorMatchRow>,
    }

    #[allow(clippy::too_many_arguments)]
    impl AdministrationStore for FakeStore {
        fn set_complimentary_premium(
            &self,
            _target_id: Uuid,
            _enabled: bool,
            reason: String,
            _actor_id: Uuid,
        ) -> AdministrationStoreFuture<'_, PremiumGrantResult> {
            Box::pin(async move {
                self.reasons
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(reason);
                Ok(PremiumGrantResult::Updated)
            })
        }

        fn user_names(
            &self,
            ids: Vec<Uuid>,
        ) -> AdministrationStoreFuture<'_, Vec<(Uuid, Option<String>)>> {
            Box::pin(async move { Ok(ids.into_iter().map(|id| (id, None)).collect()) })
        }

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
                    complimentary_premium: false,
                    is_review_account: false,
                    banned_reason: None,
                    preferences: None,
                    traits: Vec::new(),
                    consents: Vec::new(),
                    presence: None,
                    continuation_quota: None,
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
            Box::pin(async { Ok(Some(self.matches.clone())) })
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
            matches: Vec::new(),
        });
        let photos = Arc::new(Photos::default());
        (
            AdministrationService::new(
                store.clone(),
                photos.clone(),
                "terms-v1".to_owned(),
                "privacy-v1".to_owned(),
                Arc::new(crate::shared::clock::SystemClock),
            ),
            store,
            photos,
        )
    }

    #[tokio::test]
    async fn admin_match_projection_uses_the_injected_clock() {
        struct FixedClock(DateTime<Utc>);

        impl Clock for FixedClock {
            fn now(&self) -> DateTime<Utc> {
                self.0
            }
        }

        let now = Utc
            .with_ymd_and_hms(2030, 1, 1, 12, 0, 0)
            .single()
            .expect("date");
        let deadline = now + chrono::TimeDelta::days(20);
        let target = user();
        let store = FakeStore {
            user: target.clone(),
            ban_result: BanResult::Updated,
            reasons: Mutex::new(Vec::new()),
            matches: vec![CursorMatchRow {
                can_view_messages: false,
                item: crate::matches::domain::MatchRecord {
                    id: Uuid::new_v4(),
                    user1_id: target.id,
                    user2_id: Uuid::new_v4(),
                    status: crate::matches::domain::MatchStatus::Confirmed,
                    expires_at: deadline,
                    purge_after: None,
                    continuation_initiator_id: None,
                    created_at: now - chrono::TimeDelta::days(10),
                    last_message_at: None,
                },
                cursor_at: "2030-01-01T12:00:00.000000Z".to_owned(),
            }],
        };
        let service = AdministrationService::new(
            Arc::new(store),
            Arc::new(Photos::default()),
            "terms-v1".to_owned(),
            "privacy-v1".to_owned(),
            Arc::new(FixedClock(now)),
        );
        let page = service
            .matches(AuditedPageRequest {
                resource_id: target.id,
                admin_id: Uuid::new_v4(),
                admin_role: AdminRole::Superadmin,
                reason: "",
                limit: 20,
                offset: 0,
                cursor: None,
            })
            .await
            .expect("admin matches");
        assert!(page.items[0].item.archived);
        assert!(!page.items[0].can_view_messages);
        let json = serde_json::to_value(&page.items[0]).expect("administrative match");
        assert_eq!(json["can_view_messages"], false);
        assert!(json.get("item").is_none());
        assert_eq!(
            page.items[0].item.inactivity_expires_at,
            Some(crate::identity::mobile::domain::wire_timestamp(deadline))
        );
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
    async fn premium_gifts_require_superadmin_and_supply_audited_default_reasons() {
        let (service, store, _) = service(BanResult::Updated);
        let actor = Uuid::new_v4();
        let target = Uuid::new_v4();
        for enabled in [true, false] {
            assert_eq!(
                service
                    .update_complimentary_premium(target, enabled, "", actor, AdminRole::Admin)
                    .await,
                Err(AdministrationError::ActionForbidden)
            );
        }
        assert!(store.reasons.lock().expect("reasons").is_empty());
        for enabled in [true, false] {
            assert_eq!(
                service
                    .update_complimentary_premium(target, enabled, "", actor, AdminRole::Superadmin)
                    .await,
                Ok(())
            );
        }
        assert_eq!(
            store.reasons.lock().expect("reasons").as_slice(),
            [
                "Superadmin complimentary Premium grant",
                "Superadmin complimentary Premium revocation"
            ]
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

    #[tokio::test]
    async fn superadmin_uses_auditable_defaults_when_reasons_are_omitted() {
        let (service, store, _) = service(BanResult::Updated);
        let target_id = store.user.id;
        let actor_id = Uuid::new_v4();
        service
            .user_detail(target_id, actor_id, AdminRole::Superadmin, "")
            .await
            .unwrap_or_else(|_| unreachable!());
        service
            .update_ban(target_id, true, None, actor_id, AdminRole::Superadmin)
            .await
            .unwrap_or_else(|_| unreachable!());
        service
            .update_role(
                target_id,
                AdminUserRole::Admin,
                "",
                actor_id,
                AdminRole::Superadmin,
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert!(store.reasons.lock().is_ok_and(|items| items.as_slice()
            == [
                "Superadmin access",
                "Superadmin ban",
                "Superadmin role change"
            ]));

        assert!(matches!(
            service
                .user_detail(target_id, actor_id, AdminRole::Admin, "")
                .await,
            Err(AdministrationError::InvalidRequest)
        ));
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
