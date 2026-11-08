use axum::{Extension, Json};
use serde::Deserialize;

use super::{
    AdministrationHttpState, IdPath, MessageResponse, administration_error, invalid_admin_request,
    invalid_user_id,
};
use crate::administration::domain::ProfileIdentityCorrection;
use crate::http::{
    error::ApiError,
    extract::{ApiDto, ValidatedJson, ValidatedPath},
};
use crate::identity::admin::http::RecentAdminIdentity;
use crate::shared::text::validator_js_length;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CorrectProfileIdentityBody {
    firstname: String,
    birthdate: String,
    #[serde(deserialize_with = "required_nullable_sex")]
    sex: Option<crate::profiles::domain::Sex>,
    reason: String,
}

// Unlike an omitted Option, explicit null means the admin intends to clear sex.
fn required_nullable_sex<'de, D>(
    deserializer: D,
) -> Result<Option<crate::profiles::domain::Sex>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(deserializer)
}

impl ApiDto for CorrectProfileIdentityBody {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        crate::profiles::identity::strict_birthdate(&self.birthdate).is_some()
            && validator_js_length(&self.reason) <= 500
    }
}

pub(super) async fn correct_profile_identity(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedJson(body): ValidatedJson<CorrectProfileIdentityBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .correct_profile_identity(
            path.id().ok_or_else(invalid_user_id)?,
            ProfileIdentityCorrection {
                firstname: body.firstname,
                birthdate: crate::profiles::identity::strict_birthdate(&body.birthdate)
                    .ok_or_else(invalid_admin_request)?,
                sex: body.sex,
            },
            &body.reason,
            identity.user_id,
            identity.role,
        )
        .await
        .map_err(administration_error)?;
    Ok(Json(MessageResponse {
        message: "profile identity corrected",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_corrections_require_an_explicit_sex_value_and_strict_date() {
        let payload = serde_json::json!({
            "firstname": "Alice", "birthdate": "1990-01-01", "sex": null,
            "reason": "Correction requested by the account owner"
        });
        assert!(
            serde_json::from_value::<CorrectProfileIdentityBody>(payload.clone())
                .expect("explicit null")
                .is_valid()
        );
        let mut omitted = payload.clone();
        omitted.as_object_mut().expect("object").remove("sex");
        assert!(serde_json::from_value::<CorrectProfileIdentityBody>(omitted).is_err());
        for field in ["actor_role", "user_id", "is_banned"] {
            let mut extra = payload.clone();
            extra[field] = serde_json::json!(true);
            assert!(serde_json::from_value::<CorrectProfileIdentityBody>(extra).is_err());
        }
        let mut invalid = payload;
        invalid["birthdate"] = serde_json::json!("1990-02-30");
        assert!(
            !serde_json::from_value::<CorrectProfileIdentityBody>(invalid)
                .expect("body")
                .is_valid()
        );
    }
}
