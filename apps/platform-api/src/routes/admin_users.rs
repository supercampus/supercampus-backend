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
