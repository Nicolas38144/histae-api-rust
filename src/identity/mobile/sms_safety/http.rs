use axum::{Json, Router, extract::Extension, http::StatusCode, routing::get};
use serde::Deserialize;

use super::{SmsControlError, SmsSafetyService, SmsSafetySnapshot};
use crate::{
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson},
        router::HttpState,
    },
    identity::{
        admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity},
        admin_role::AdminRole,
    },
};

pub fn routes(service: SmsSafetyService, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/sms-safety", get(snapshot).patch(set_suspended))
        .layer(Extension(service))
        .layer(Extension(auth))
}

async fn snapshot(
    AdminIdentity(_): AdminIdentity,
    Extension(service): Extension<SmsSafetyService>,
) -> Result<Json<SmsSafetySnapshot>, ApiError> {
    service.snapshot().await.map(Json).map_err(Into::into)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuspensionBody {
    suspended: bool,
    version: i64,
}

impl ApiDto for SuspensionBody {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";
    fn is_valid(&self) -> bool {
        self.version >= 0
    }
}

async fn set_suspended(
    RecentAdminIdentity(actor): RecentAdminIdentity,
    Extension(service): Extension<SmsSafetyService>,
    ValidatedJson(body): ValidatedJson<SuspensionBody>,
) -> Result<StatusCode, ApiError> {
    if actor.role != AdminRole::Superadmin {
        return Err(forbidden());
    }
    service
        .set_suspended(actor.user_id, body.suspended, body.version)
        .await
        .map_err(|error| match error {
            SmsControlError::Forbidden => forbidden(),
            SmsControlError::Conflict => ApiError::new(
                StatusCode::CONFLICT,
                "resource_changed",
                "This resource has changed. Reload it before making a decision.",
            ),
            SmsControlError::Database(error) => error.into(),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "admin_action_forbidden",
        "This administrator action is not allowed.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::SecretString,
        identity::{
            admin::service::AuthenticatedAdmin,
            mobile::sms_safety::{SmsSafetyFuture, SmsSafetyStore, SmsSendRejection, SmsUsage},
        },
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use uuid::Uuid;

    struct Store(Arc<AtomicUsize>);
    impl SmsSafetyStore for Store {
        fn reserve<'a>(
            &'a self,
            _: Uuid,
            _: String,
            _: crate::config::SmsSafetyConfig,
        ) -> SmsSafetyFuture<'a, Result<(), SmsSendRejection>> {
            Box::pin(async { Ok(Ok(())) })
        }
        fn usage(&self) -> SmsSafetyFuture<'_, SmsUsage> {
            Box::pin(async { Err(crate::infra::postgres::DatabaseError::QueryFailed) })
        }
        fn set_suspended(
            &self,
            _: Uuid,
            _: bool,
            _: i64,
        ) -> SmsSafetyFuture<'_, (), SmsControlError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(SmsControlError::Conflict) })
        }
    }

    #[tokio::test]
    async fn ordinary_admin_cannot_change_controls_and_a_stale_superadmin_view_is_rejected() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = SmsSafetyService::new(
            Arc::new(Store(calls.clone())),
            Default::default(),
            SecretString::new("h".repeat(32)),
            true,
        );
        let actor = |role| {
            RecentAdminIdentity(AuthenticatedAdmin {
                user_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                credential_id: Uuid::new_v4(),
                role,
                authenticated_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::TimeDelta::minutes(10),
            })
        };
        let error = set_suspended(
            actor(AdminRole::Admin),
            Extension(service.clone()),
            ValidatedJson(SuspensionBody {
                suspended: true,
                version: 0,
            }),
        )
        .await
        .expect_err("admin forbidden");
        assert_eq!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let error = set_suspended(
            actor(AdminRole::Superadmin),
            Extension(service),
            ValidatedJson(SuspensionBody {
                suspended: false,
                version: 0,
            }),
        )
        .await
        .expect_err("stale version");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
