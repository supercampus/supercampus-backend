use axum::{Extension, Json, extract::Path, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::require_effective_permission;
use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess, MINIMUM_PASSWORD_LENGTH},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetTenantUserPasswordRequest {
    password: String,
}

pub(super) async fn set_tenant_user_password(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<SetTenantUserPasswordRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;
    if request.password.chars().count() < MINIMUM_PASSWORD_LENGTH || request.password.len() > 72 {
        return Err(ApiError::BadRequest(format!(
            "password must be at least {MINIMUM_PASSWORD_LENGTH} characters and no more than 72 bytes"
        )));
    }
    let changed = state
        .set_tenant_user_password(
            &principal.student.tenant_id,
            &principal.student.id,
            user_id,
            &request.password,
        )
        .await?;
    if !changed {
        return Err(ApiError::NotFound("tenant user not found".into()));
    }
    state.publish_realtime(
        RealtimePublication::tenant(
            principal.student.tenant_id,
            "identity.password.changed",
            json!({"userId": user_id}),
        )
        .for_user(user_id.to_string()),
    );
    Ok(Json(ApiResponse::new(json!({
        "userId": user_id,
        "sessionsRevoked": true
    }))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateTenantUserRequest {
    pub name: Option<String>,
    pub email: Option<String>,
}

pub(super) async fn update_tenant_user(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<UpdateTenantUserRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;

    let name = request.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let email = request.email.as_deref().map(str::trim).filter(|s| !s.is_empty());

    if name.is_none() && email.is_none() {
        return Err(ApiError::BadRequest(
            "at least name or email must be provided to update".into(),
        ));
    }

    if let Some(ref em) = email {
        if !em.contains('@') || !em.contains('.') {
            return Err(ApiError::BadRequest(
                "a valid email address is required".into(),
            ));
        }
    }

    let updated = state
        .update_tenant_user_profile(
            &principal.student.tenant_id,
            &principal.student.id,
            user_id,
            name,
            email,
        )
        .await?;

    let Some(user) = updated else {
        return Err(ApiError::NotFound("tenant user not found".into()));
    };

    state.publish_realtime(
        RealtimePublication::tenant(
            principal.student.tenant_id,
            "identity.user.updated",
            user.clone(),
        )
        .for_user(user_id.to_string()),
    );

    Ok(Json(ApiResponse::new(user)))
}

/// A student's year of study must be 1–6.
pub(super) fn is_valid_student_year(year: Option<u8>) -> bool {
    year.is_some_and(|year| (1..=6).contains(&year))
}

/// Whether any of `(role_key, portal_family)` makes the account a student
/// record, which needs a year of study. This validates data, not access.
pub(super) fn assigns_student_role(roles: &[(String, String)]) -> bool {
    roles.iter().any(|(key, family)| {
        key.eq_ignore_ascii_case("student") || family.eq_ignore_ascii_case("student")
    })
}

/// Rejects creating a student account without a valid year of study (1–6).
pub(super) async fn require_student_year(
    state: &AppState,
    tenant_slug: &str,
    role_ids: &[Uuid],
    year_of_study: Option<u8>,
) -> ApiResult<()> {
    if let Some(year) = year_of_study
        && !(1..=6).contains(&year)
    {
        return Err(ApiError::BadRequest(
            "Year of study must be between 1 and 6".into(),
        ));
    }
    let roles = state.tenant_role_descriptors(tenant_slug, role_ids).await?;
    if assigns_student_role(&roles) && !is_valid_student_year(year_of_study) {
        return Err(ApiError::BadRequest(
            "Year of study (1–6) is required for a student".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetTenantUserYearRequest {
    year_of_study: Option<u8>,
}

/// `PUT /authorization/users/{user_id}/year` — set a student's year of study
/// (1–6) on the membership, the account and the linked student record.
pub(super) async fn set_tenant_user_year(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<SetTenantUserYearRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;
    let Some(year) = request.year_of_study.filter(|year| (1..=6).contains(year)) else {
        return Err(ApiError::BadRequest(
            "Year of study must be between 1 and 6".into(),
        ));
    };
    let updated = state
        .set_tenant_user_year(
            &principal.student.tenant_id,
            &principal.student.id,
            user_id,
            year,
        )
        .await?;
    let Some(result) = updated else {
        return Err(ApiError::NotFound("tenant user not found".into()));
    };
    state.publish_realtime(
        RealtimePublication::tenant(
            principal.student.tenant_id,
            "identity.user.updated",
            result.clone(),
        )
        .for_user(user_id.to_string()),
    );
    Ok(Json(ApiResponse::new(result)))
}

/// Roles that carry authority beyond a tenant administrator's: the platform's
/// own roles and the tenant-level super administrator, which the API treats as
/// an unconditional override in several places.
pub(super) fn is_privileged_role(role_key: &str, portal_family: &str) -> bool {
    let key = role_key.trim().to_ascii_lowercase();
    matches!(key.as_str(), "superadmin" | "super_admin")
        || key.starts_with("platform_")
        || portal_family.trim().eq_ignore_ascii_case("platform-control")
}

/// A privileged role may only be granted or taken away by a platform
/// administrator, or by someone who already holds that same role. Anyone else
/// (a tenant admin included) would be escalating privilege.
pub(super) fn may_change_role(
    actor_roles: &[String],
    actor_is_platform_admin: bool,
    role_key: &str,
    portal_family: &str,
) -> bool {
    !is_privileged_role(role_key, portal_family)
        || actor_is_platform_admin
        || actor_roles.iter().any(|held| held == role_key)
}

/// The first role the actor may not add or remove when replacing `current`
/// with `requested`, if any. Each entry is `(role_key, portal_family)`.
pub(super) fn forbidden_role_change<'a>(
    actor_roles: &[String],
    actor_is_platform_admin: bool,
    current: &'a [(String, String)],
    requested: &'a [(String, String)],
) -> Option<&'a str> {
    let added = requested
        .iter()
        .filter(|(key, _)| !current.iter().any(|(held, _)| held == key));
    let removed = current
        .iter()
        .filter(|(key, _)| !requested.iter().any(|(kept, _)| kept == key));
    added
        .chain(removed)
        .find(|(key, family)| !may_change_role(actor_roles, actor_is_platform_admin, key, family))
        .map(|(key, _)| key.as_str())
}

/// Whether the actor may manage (deactivate, re-role) a user holding
/// `target_roles`: a tenant admin must not lock out a super administrator.
pub(super) fn may_manage_user(
    actor_roles: &[String],
    actor_is_platform_admin: bool,
    target_roles: &[(String, String)],
) -> bool {
    target_roles
        .iter()
        .all(|(key, family)| may_change_role(actor_roles, actor_is_platform_admin, key, family))
}

pub(super) async fn guard_role_assignment(
    state: &AppState,
    tenant_slug: &str,
    access: &EffectiveAccess,
    user_id: Option<Uuid>,
    role_ids: &[Uuid],
) -> ApiResult<()> {
    let requested = state.tenant_role_descriptors(tenant_slug, role_ids).await?;
    let current = match user_id {
        Some(user_id) => state.tenant_user_role_descriptors(tenant_slug, user_id).await?,
        None => Vec::new(),
    };
    let platform_admin = crate::platform_admin::is_platform_admin(access);
    if let Some(role) = forbidden_role_change(&access.roles, platform_admin, &current, &requested) {
        return Err(ApiError::ForbiddenWithMessage(format!(
            "The {role} role can only be assigned or removed by a platform administrator"
        )));
    }
    Ok(())
}

/// Creating, renaming, re-familying, re-permissioning or deleting a role is
/// held to the same rule as assigning it: a tenant administrator must not be
/// able to shape a privileged role (for example give the super administrator
/// role new grants, or move a role into the platform-control family).
///
/// `existing_role` is the role being changed, if any; `new_key` and
/// `new_family` are values the request would set.
pub(super) async fn guard_role_definition(
    state: &AppState,
    tenant_slug: &str,
    access: &EffectiveAccess,
    existing_role: Option<Uuid>,
    new_key: Option<&str>,
    new_family: Option<&str>,
) -> ApiResult<()> {
    let platform_admin = crate::platform_admin::is_platform_admin(access);
    if let Some(role_id) = existing_role {
        let existing = state.tenant_role_descriptors(tenant_slug, &[role_id]).await?;
        if let Some((key, _)) = existing
            .iter()
            .find(|(key, family)| !may_change_role(&access.roles, platform_admin, key, family))
        {
            return Err(ApiError::ForbiddenWithMessage(format!(
                "The {key} role can only be changed by a platform administrator"
            )));
        }
    }
    if new_key.is_some() || new_family.is_some() {
        let key = new_key.unwrap_or_default();
        let family = new_family.unwrap_or_default();
        if !may_change_role(&access.roles, platform_admin, key, family) {
            return Err(ApiError::ForbiddenWithMessage(
                "Only a platform administrator can create or configure platform roles".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetTenantUserStatusRequest {
    active: bool,
}

/// `PUT /authorization/users/{user_id}/status` — deactivate or reactivate a
/// member of the caller's tenant. Deactivation signs the user out everywhere
/// in this tenant.
pub(super) async fn set_tenant_user_status(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<SetTenantUserStatusRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;
    if Uuid::parse_str(&principal.student.id).ok() == Some(user_id) {
        return Err(ApiError::BadRequest(
            "You cannot change the status of your own account".into(),
        ));
    }
    let tenant_slug = &principal.student.tenant_id;
    let target_roles = state
        .tenant_user_role_descriptors(tenant_slug, user_id)
        .await?;
    if !may_manage_user(
        &access.roles,
        crate::platform_admin::is_platform_admin(&access),
        &target_roles,
    ) {
        return Err(ApiError::ForbiddenWithMessage(
            "Only a platform administrator can change this account's status".into(),
        ));
    }
    let changed = state
        .set_tenant_user_active(tenant_slug, &principal.student.id, user_id, request.active)
        .await?;
    if !changed {
        return Err(ApiError::NotFound("tenant user not found".into()));
    }
    let event = if request.active {
        "identity.user.reactivated"
    } else {
        "identity.user.deactivated"
    };
    state.publish_realtime(
        RealtimePublication::tenant(
            principal.student.tenant_id.clone(),
            event,
            json!({"userId": user_id, "active": request.active}),
        )
        .for_user(user_id.to_string()),
    );
    Ok(Json(ApiResponse::new(json!({
        "userId": user_id,
        "active": request.active,
        "sessionsRevoked": !request.active
    }))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(key: &str, family: &str) -> (String, String) {
        (key.into(), family.into())
    }

    #[test]
    fn tenant_admin_cannot_grant_super_administrator() {
        let tenant_admin = vec!["tenant_admin".to_string()];
        let current = vec![role("staff", "staff")];
        let requested = vec![role("staff", "staff"), role("superadmin", "admin")];
        assert_eq!(
            forbidden_role_change(&tenant_admin, false, &current, &requested),
            Some("superadmin")
        );
        // Nor strip it from someone who holds it.
        assert_eq!(
            forbidden_role_change(&tenant_admin, false, &requested, &current),
            Some("superadmin")
        );
    }

    #[test]
    fn platform_roles_are_privileged_by_key_or_family() {
        assert!(is_privileged_role("platform_super_admin", "admin"));
        assert!(is_privileged_role("ops", "platform-control"));
        assert!(is_privileged_role("SuperAdmin", "admin"));
        assert!(!is_privileged_role("tenant_admin", "staff"));
        assert!(!is_privileged_role("accountant", "admin"));
    }

    #[test]
    fn ordinary_roles_and_authorised_actors_pass() {
        let tenant_admin = vec!["tenant_admin".to_string()];
        let current = vec![role("staff", "staff")];
        let requested = vec![role("accountant", "admin")];
        assert_eq!(
            forbidden_role_change(&tenant_admin, false, &current, &requested),
            None
        );
        let privileged = vec![role("superadmin", "admin")];
        assert_eq!(
            forbidden_role_change(&tenant_admin, true, &current, &privileged),
            None
        );
        let superadmin = vec!["superadmin".to_string()];
        assert_eq!(
            forbidden_role_change(&superadmin, false, &current, &privileged),
            None
        );
    }

    #[test]
    fn tenant_admin_cannot_manage_a_super_administrator() {
        let tenant_admin = vec!["tenant_admin".to_string()];
        assert!(!may_manage_user(&tenant_admin, false, &[role("superadmin", "admin")]));
        assert!(may_manage_user(&tenant_admin, false, &[role("accountant", "admin")]));
    }

    #[test]
    fn deserializes_update_request() {
        let json = r#"{"name": "Jane Doe", "email": "jane@mec.local"}"#;
        let req: UpdateTenantUserRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name.as_deref(), Some("Jane Doe"));
        assert_eq!(req.email.as_deref(), Some("jane@mec.local"));

        let json_name_only = r#"{"name": "Jane Smith"}"#;
        let req2: UpdateTenantUserRequest = serde_json::from_str(json_name_only).unwrap();
        assert_eq!(req2.name.as_deref(), Some("Jane Smith"));
        assert_eq!(req2.email, None);
    }

    #[test]
    fn student_year_must_be_one_to_six() {
        assert!(!is_valid_student_year(None));
        assert!(!is_valid_student_year(Some(0)));
        assert!(!is_valid_student_year(Some(7)));
        for year in 1..=6 {
            assert!(is_valid_student_year(Some(year)));
        }
    }

    #[test]
    fn student_role_is_detected_by_key_or_family() {
        assert!(assigns_student_role(&[role("student", "student")]));
        assert!(assigns_student_role(&[role("ug_student", "student")]));
        assert!(!assigns_student_role(&[role("staff", "staff")]));
        assert!(!assigns_student_role(&[]));
    }

    #[test]
    fn year_request_rejects_unknown_fields() {
        let req: SetTenantUserYearRequest =
            serde_json::from_str(r#"{"yearOfStudy": 3}"#).unwrap();
        assert_eq!(req.year_of_study, Some(3));
        assert!(serde_json::from_str::<SetTenantUserYearRequest>(r#"{"year": 3}"#).is_err());
    }
}


