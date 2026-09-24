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

#[cfg(test)]
mod tests {
    use super::*;

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
}


