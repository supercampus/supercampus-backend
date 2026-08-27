use std::collections::HashSet;

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::{
        ApiResponse, AssignUserRolesRequest, BootstrapDocument, BulkStudentImportRequest,
        CreateAuthorizationRoleRequest, CreateRecordRequest, CreateTenantUserRequest,
        ForgotPasswordRequest, HealthDocument, LoginData, LoginRequest, LogoutRequest,
        NavigationItem, PutConfigurationRequest, RefreshRequest, ResetPasswordRequest,
        SaveAppStateRequest, SessionData, SessionMode, SetRolePermissionsRequest,
        SetUserAccessRequest, StudentPhotoRequest, UpdateAuthorizationRoleRequest,
        UpdateRecordRequest, ValidateWorkflowTransitionRequest,
    },
    realtime::RealtimePublication,
    state::{
        AccessTokenAuthentication, AppState, AuthPrincipal, CreatedAuthSession, EffectiveAccess,
        MINIMUM_PASSWORD_LENGTH, RefreshSessionResult,
    },
};

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/", get(api_index))
        .route("/bootstrap", get(bootstrap))
        .route("/services", get(list_services))
        .route("/services/{service_key}", get(get_service))
        .route("/modules", get(list_modules))
        .route("/modules/{module_key}", get(get_module))
        .route(
            "/authorization/permissions",
            get(list_authorization_permissions),
        )
        .route(
            "/authorization/roles",
            get(list_authorization_roles).post(create_authorization_role),
        )
        .route(
            "/authorization/roles/{role_id}",
            put(update_authorization_role).delete(delete_authorization_role),
        )
        .route(
            "/authorization/roles/{role_id}/permissions",
            put(set_authorization_role_permissions),
        )
        .route(
            "/authorization/users",
            get(list_tenant_users).post(create_tenant_user),
        )
        .route(
            "/authorization/users/{user_id}/roles",
            put(assign_tenant_user_roles),
        )
        .route(
            "/authorization/users/{user_id}/access",
            get(get_tenant_user_access).put(set_tenant_user_access),
        )
        .route(
            "/configuration/{namespace}",
            get(get_configuration).put(put_configuration),
        )
        .route(
            "/workflows/{module_key}/{feature_key}",
            get(get_workflow_definition),
        )
        .route(
            "/workflows/{module_key}/{feature_key}/transitions/validate",
            post(validate_workflow_transition),
        )
        .route("/navigation", get(get_navigation))
        .route("/dashboard/effective", get(get_effective_dashboard))
        .route("/student-master", get(list_student_master))
        .route("/student-master/import", post(import_student_master))
        .route("/student-master/{student_id}/photo", put(set_student_photo))
        // Reached by a guardian holding a WhatsApp link and nothing else.
        // Exempted from authorization in `requires_authorization` below.
        .route(
            "/public/gatepass/approvals/{token}",
            get(crate::guardian_link::show_guardian_request)
                .post(crate::guardian_link::decide_as_guardian),
        )
        .route("/realtime/ws", get(crate::realtime::websocket))
        .nest(
            "/academic-assignments",
            crate::academic_assignments::router(),
        )
        .nest("/timetable", crate::timetable::router())
        .nest("/operations", crate::operations::router())
        .route(
            "/{module_key}/records",
            get(list_records).post(create_record),
        )
        .route(
            "/{module_key}/records/{record_id}",
            get(get_record).patch(update_record).delete(delete_record),
        );

    let api = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/forgot-password", post(forgot_password))
        .route("/auth/reset-password", post(reset_password))
        .route("/auth/refresh", post(refresh))
        .route("/auth/realtime-token", post(realtime_token))
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/state", get(get_app_state).put(save_app_state))
        .route(
            "/media/upload",
            post(upload_media).layer(DefaultBodyLimit::max(crate::media::MULTIPART_BODY_LIMIT)),
        )
        .route(
            "/media/public-application/{token}",
            post(upload_public_application_media)
                .layer(DefaultBodyLimit::max(crate::media::MULTIPART_BODY_LIMIT)),
        )
        .nest("/v1", v1);

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .nest("/api", api)
        .with_state(state)
}

async fn health() -> Json<HealthDocument> {
    Json(HealthDocument {
        status: "ok",
        service: "supercampus-platform-api",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn ready(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    state.ready().await?;
    Ok(Json(json!({
        "status": "ready",
        "checks": { "runtime": "ok", "storage": state.storage_kind() }
    })))
}

async fn upload_media(
    Extension(principal): Extension<AuthPrincipal>,
    multipart: Multipart,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    let media = crate::media::upload(&principal.student.tenant_id, multipart).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(media))))
}

async fn upload_public_application_media(
    State(state): State<AppState>,
    Path(token): Path<Uuid>,
    headers: HeaderMap,
    multipart: Multipart,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    let tenant = headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    let verification = headers
        .get("x-application-verification")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    let mut hasher = Sha256::new();
    hasher.update(tenant.as_bytes());
    hasher.update(b":");
    hasher.update(verification.as_bytes());
    let verification_hash = format!("{:x}", hasher.finalize());
    let database = state.tenant_database(tenant).await.map_err(|error| {
        tracing::error!(tenant, %error, "public application tenant database resolution failed");
        ApiError::ServiceUnavailable("Application media storage is temporarily unavailable".into())
    })?;
    let authorized = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM crm.application_invitations WHERE tenant_id = (SELECT id FROM platform.tenants WHERE slug = $1) AND token = $2 AND status = 'verified' AND verification_token_hash = $3 AND verified_at IS NOT NULL AND submitted_at IS NULL AND expires_at > now())",
    )
    .bind(tenant)
    .bind(token)
    .bind(verification_hash)
    .fetch_one(database.pool())
    .await
    .map_err(|error| {
        tracing::error!(tenant, %error, "public application upload authorization query failed");
        ApiError::Internal
    })?;
    if !authorized {
        return Err(ApiError::Unauthorized);
    }
    let media = crate::media::upload(tenant, multipart).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(media))))
}

async fn api_index(State(state): State<AppState>) -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(json!({
        "name": "SuperCampus Platform API",
        "version": env!("CARGO_PKG_VERSION"),
        "services": state.services().len(),
        "modules": state.modules().len(),
        "documentation": "/api/v1/services"
    })))
}

async fn list_services(State(state): State<AppState>) -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(json!(state.services())))
}

async fn get_service(
    State(state): State<AppState>,
    Path(service_key): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let service = state
        .services()
        .into_iter()
        .find(|service| service.key == service_key)
        .ok_or_else(|| ApiError::NotFound(format!("Unknown service: {service_key}")))?;
    Ok(Json(ApiResponse::new(json!(service))))
}

async fn bootstrap(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<BootstrapDocument>>> {
    let tenant_brand = state
        .configuration(&principal.student.tenant_id, "tenant-branding")
        .await?
        .map(|document| document.value)
        .unwrap_or_else(|| default_tenant_brand(&principal));
    let modules = state
        .modules()
        .into_iter()
        .filter(|module| can_access_module(&access, &module.key))
        .collect::<Vec<_>>();
    let navigation = modules
        .iter()
        .filter_map(|module| {
            module_read_permission(&access, &module.key).map(|required_permission| NavigationItem {
                id: module.key.clone(),
                label: module.name.clone(),
                route: format!("/dashboard/{}", module.key),
                required_permission,
            })
        })
        .collect();
    let mut workflows = Vec::new();
    if access.allows("gatepass.outpass.read")
        && let Some(workflow) = state
            .workflow_definition(&principal.student.tenant_id, "gatepass", "outpass")
            .await?
    {
        workflows.push(workflow);
    }
    Ok(Json(ApiResponse::new(BootstrapDocument {
        tenant_id: principal.student.tenant_id,
        user_id: principal.student.id,
        tenant_brand,
        roles: access.roles,
        portal_families: access.portal_families,
        permissions: access.permissions,
        permission_scopes: access.scopes,
        workflows,
        services: state.services(),
        modules,
        navigation,
    })))
}

async fn list_modules(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> Json<ApiResponse<Value>> {
    let modules = state
        .modules()
        .into_iter()
        .filter(|module| can_access_module(&access, &module.key))
        .collect::<Vec<_>>();
    Json(ApiResponse::new(json!(modules)))
}

async fn get_module(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path(module_key): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let module = ensure_module(&state, &module_key)?;
    if !can_access_module(&access, &module_key) {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(ApiResponse::new(json!(module))))
}

async fn list_authorization_permissions(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.permissions.read")?;
    Ok(Json(ApiResponse::new(
        state
            .authorization_permissions(&principal.student.tenant_id)
            .await?,
    )))
}

async fn list_authorization_roles(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.roles.read")?;
    Ok(Json(ApiResponse::new(
        state
            .authorization_roles(&principal.student.tenant_id)
            .await?,
    )))
}

async fn create_authorization_role(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateAuthorizationRoleRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_effective_permission(&access, "authorization.roles.create")?;
    if request.name.trim().is_empty() || !valid_role_key(&request.key) {
        return Err(ApiError::BadRequest(
            "name is required and key must use lowercase letters, numbers, or underscores".into(),
        ));
    }
    if request.surfaces.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one role surface is required".into(),
        ));
    }
    for surface in &request.surfaces {
        validate_surface(surface)?;
    }
    let role = state
        .create_authorization_role(
            &principal.student.tenant_id,
            &principal.student.id,
            &request,
        )
        .await?;
    state.publish_realtime(RealtimePublication::tenant(
        principal.student.tenant_id,
        "authorization.changed",
        json!({"resource": "role", "action": "created"}),
    ));
    Ok((StatusCode::CREATED, Json(ApiResponse::new(role))))
}

async fn update_authorization_role(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(role_id): Path<Uuid>,
    Json(request): Json<UpdateAuthorizationRoleRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.roles.update")?;
    Ok(Json(ApiResponse::new(
        state
            .update_authorization_role(
                &principal.student.tenant_id,
                &principal.student.id,
                role_id,
                &request,
            )
            .await?,
    )))
}

async fn delete_authorization_role(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(role_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require_effective_permission(&access, "authorization.roles.delete")?;
    state
        .delete_authorization_role(&principal.student.tenant_id, &principal.student.id, role_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_authorization_role_permissions(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(role_id): Path<Uuid>,
    Json(request): Json<SetRolePermissionsRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.roles.update")?;
    validate_surface(&request.surface)?;
    if request
        .permissions
        .iter()
        .any(|grant| !valid_permission_scope(&grant.scope))
    {
        return Err(ApiError::BadRequest(
            "permission scope must be own, assigned, department, institution, or all".into(),
        ));
    }
    let mut permission_keys = HashSet::new();
    if request.permissions.iter().any(|grant| {
        let key = grant.key.trim();
        key.is_empty() || !permission_keys.insert(key.to_owned())
    }) {
        return Err(ApiError::BadRequest(
            "permission keys must be non-empty and unique".into(),
        ));
    }
    let permission_keys = permission_keys.into_iter().collect::<Vec<_>>();
    if !state
        .authorization_permission_keys_exist(&principal.student.tenant_id, &permission_keys)
        .await?
    {
        return Err(ApiError::BadRequest(
            "one or more permissions are inactive or do not belong to this tenant".into(),
        ));
    }
    let result = state
        .set_authorization_role_permissions(
            &principal.student.tenant_id,
            &principal.student.id,
            role_id,
            &request.surface,
            &request.permissions,
        )
        .await?;
    state.publish_realtime(RealtimePublication::tenant(
        principal.student.tenant_id,
        "authorization.changed",
        json!({"resource": "role_permissions", "roleId": role_id, "surface": request.surface}),
    ));
    Ok(Json(ApiResponse::new(result)))
}

async fn list_tenant_users(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.read")?;
    Ok(Json(ApiResponse::new(
        state.tenant_users(&principal.student.tenant_id).await?,
    )))
}

async fn create_tenant_user(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateTenantUserRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_effective_permission(&access, "authorization.users.create")?;
    if request.name.trim().is_empty()
        || !request.email.contains('@')
        || request.role_ids.is_empty()
        || request
            .credential_password()
            .is_none_or(|password| password.chars().count() < 12 || password.len() > 72)
    {
        return Err(ApiError::BadRequest(
            "name, valid email, at least one role, and a password between 12 characters and 72 bytes are required".into(),
        ));
    }
    let user = state
        .create_tenant_user(
            &principal.student.tenant_id,
            &principal.student.id,
            &request,
        )
        .await?
        .ok_or_else(|| {
            ApiError::Conflict(
                "a user with this email already belongs to the tenant; assign roles to the existing user instead".into(),
            )
        })?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(user))))
}

async fn assign_tenant_user_roles(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<AssignUserRolesRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;
    if request.role_ids.is_empty() {
        return Err(ApiError::BadRequest("at least one role is required".into()));
    }
    Ok(Json(ApiResponse::new(
        state
            .assign_tenant_user_roles(
                &principal.student.tenant_id,
                &principal.student.id,
                user_id,
                &request,
            )
            .await?,
    )))
}

#[derive(Debug, serde::Deserialize)]
struct AccessSurfaceQuery {
    surface: Option<String>,
}

async fn get_tenant_user_access(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<AccessSurfaceQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.read")?;
    let surface = query.surface.unwrap_or_else(|| "app".into());
    validate_surface(&surface)?;
    Ok(Json(ApiResponse::new(
        state
            .tenant_user_access(&principal.student.tenant_id, user_id, &surface)
            .await?,
    )))
}

async fn set_tenant_user_access(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<SetUserAccessRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "authorization.users.update")?;
    validate_surface(&request.surface)?;
    if request.grants.iter().any(|grant| {
        !valid_permission_scope(&grant.scope)
            || !matches!(grant.mode.as_str(), "allow" | "deny")
            || grant.key.trim().is_empty()
    }) {
        return Err(ApiError::BadRequest(
            "each grant needs a non-empty key, valid scope, and valid mode".into(),
        ));
    }
    let keys = request
        .grants
        .iter()
        .map(|grant| grant.key.trim().to_owned())
        .collect::<HashSet<_>>();
    if keys.len() != request.grants.len() {
        return Err(ApiError::BadRequest(
            "permission keys must be unique".into(),
        ));
    }
    if !state
        .authorization_permission_keys_exist(
            &principal.student.tenant_id,
            &keys.iter().cloned().collect::<Vec<_>>(),
        )
        .await?
    {
        return Err(ApiError::BadRequest(
            "one or more permissions are inactive or do not belong to this tenant".into(),
        ));
    }
    let result = state
        .set_tenant_user_access(
            &principal.student.tenant_id,
            &principal.student.id,
            user_id,
            &request,
        )
        .await?;
    state.publish_realtime(
        RealtimePublication::tenant(
            principal.student.tenant_id,
            "authorization.changed",
            json!({"resource": "user_access", "userId": user_id, "surface": request.surface}),
        )
        .for_user(user_id.to_string()),
    );
    Ok(Json(ApiResponse::new(result)))
}

fn validate_surface(surface: &str) -> ApiResult<()> {
    if matches!(surface, "app" | "website") {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "surface must be app or website".into(),
        ))
    }
}

fn require_effective_permission(access: &EffectiveAccess, permission: &str) -> ApiResult<()> {
    if access.allows(permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn valid_role_key(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

async fn list_student_master(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "students.directory.read")?;
    let students = state
        .list_student_master(&principal.student.tenant_id)
        .await?;
    Ok(Json(ApiResponse::new(students)))
}

/// Sets or clears a student's photograph.
///
/// The image itself is uploaded to `/v1/media/upload` first, which stores it in
/// the tenant's own Cloudinary folder and hands back a URL. Only that URL
/// arrives here, so this endpoint never handles file bytes and cannot be used
/// to attach media belonging to another tenant.
async fn set_student_photo(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(student_id): Path<Uuid>,
    Json(request): Json<StudentPhotoRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "students.directory.create")?;

    let photo_url = request
        .photo_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(url) = photo_url {
        // Anything that is not an https URL has no business being rendered as a
        // student's face in the app.
        if !url.starts_with("https://") {
            return Err(ApiError::BadRequest(
                "A student photograph must be an https URL".into(),
            ));
        }
        if url.len() > 2048 {
            return Err(ApiError::BadRequest(
                "That photograph URL is too long".into(),
            ));
        }
    }

    let updated = state
        .set_student_photo(&principal.student.tenant_id, student_id, photo_url)
        .await?
        .ok_or_else(|| ApiError::NotFound("Student not found".into()))?;

    Ok(Json(ApiResponse::new(updated)))
}

async fn import_student_master(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<BulkStudentImportRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "students.directory.create")?;
    if request.rows.is_empty() || request.rows.len() > 1_000 {
        return Err(ApiError::BadRequest(
            "Provide between 1 and 1000 student rows".into(),
        ));
    }

    let mut seen_roll_numbers = HashSet::new();
    for (index, row) in request.rows.iter().enumerate() {
        if row.name.trim().is_empty()
            || row.roll_no.trim().is_empty()
            || row.department.trim().is_empty()
            || row.mobile_number.trim().is_empty()
            || row.email.trim().is_empty()
        {
            return Err(ApiError::BadRequest(format!(
                "Row {} has a missing required value",
                index + 2
            )));
        }
        let roll_no = row.roll_no.trim().to_ascii_lowercase();
        if !seen_roll_numbers.insert(roll_no) {
            return Err(ApiError::BadRequest(format!(
                "Row {} repeats a roll number",
                index + 2
            )));
        }
        if !row.email.contains('@') {
            return Err(ApiError::BadRequest(format!(
                "Row {} has an invalid email",
                index + 2
            )));
        }
    }

    let result = state
        .import_student_master(
            &principal.student.tenant_id,
            &principal.student.id,
            &request.rows,
        )
        .await?;
    Ok(Json(ApiResponse::new(result)))
}

async fn list_records(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path(module_key): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    require_module_record_permission(&access, &module_key, "read")?;
    let records = state
        .list_records(&tenant_id(&headers), &module_key)
        .await?;
    Ok(Json(ApiResponse::new(json!(records))))
}

async fn create_record(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path(module_key): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateRecordRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    ensure_module(&state, &module_key)?;
    require_module_record_permission(&access, &module_key, "create")?;
    if request.record_type.trim().is_empty() {
        return Err(ApiError::BadRequest("recordType is required".into()));
    }
    let record = state
        .create_record(
            tenant_id(&headers),
            module_key,
            request.record_type,
            request.data,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(json!(record)))))
}

async fn get_record(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    require_module_record_permission(&access, &module_key, "read")?;
    let record = state
        .record(&tenant_id(&headers), &module_key, record_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Record not found: {record_id}")))?;
    Ok(Json(ApiResponse::new(json!(record))))
}

async fn update_record(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<UpdateRecordRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    require_module_record_permission(&access, &module_key, "update")?;
    let record = state
        .update_record(&tenant_id(&headers), &module_key, record_id, request.data)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Record not found: {record_id}")))?;
    Ok(Json(ApiResponse::new(json!(record))))
}

async fn delete_record(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    ensure_module(&state, &module_key)?;
    require_module_record_permission(&access, &module_key, "delete")?;
    if state
        .delete_record(&tenant_id(&headers), &module_key, record_id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("Record not found: {record_id}")))
    }
}

async fn get_configuration(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(namespace): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    // Tenant branding drives every authenticated surface, including restricted
    // student and vendor accounts. Other configuration namespaces remain gated.
    if namespace != "tenant-branding" {
        require_effective_permission(&access, "platform.configuration.read")?;
    }
    let document = state
        .configuration(&principal.student.tenant_id, &namespace)
        .await?;
    let response = match document {
        Some(document) => json!(document),
        None if namespace == "tenant-branding" => json!({
            "tenantId": principal.student.tenant_id,
            "namespace": namespace,
            "version": 0,
            "value": default_tenant_brand(&principal),
            "updatedAt": Utc::now(),
        }),
        None => {
            return Err(ApiError::NotFound(format!(
                "Configuration not found: {namespace}"
            )));
        }
    };
    Ok(Json(ApiResponse::new(response)))
}

fn default_tenant_brand(principal: &AuthPrincipal) -> Value {
    json!({
        "collegeName": principal.student.tenant.name,
        "suiteName": "Admin Suite",
        "logoDataUrl": null,
        "primary": "#1A6B3C",
        "secondary": "#F5A623",
        "surface": "#EAF5EE"
    })
}

async fn put_configuration(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(namespace): Path<String>,
    Json(request): Json<PutConfigurationRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, "platform.configuration.update")?;
    if namespace.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "Configuration namespace is required".into(),
        ));
    }
    let document = state
        .put_configuration(
            principal.student.tenant_id.clone(),
            namespace.clone(),
            request.value,
        )
        .await?;
    state.publish_realtime(RealtimePublication::tenant(
        principal.student.tenant_id,
        "configuration.changed",
        json!({"namespace": namespace, "version": document.version}),
    ));
    Ok(Json(ApiResponse::new(json!(document))))
}

async fn get_workflow_definition(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path((module_key, feature_key)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    require_effective_permission(&access, &format!("{module_key}.{feature_key}.read"))?;
    let definition = state
        .workflow_definition(&tenant_id(&headers), &module_key, &feature_key)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!("Workflow not found: {module_key}.{feature_key}"))
        })?;
    Ok(Json(ApiResponse::new(json!(definition))))
}

async fn validate_workflow_transition(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path((module_key, feature_key)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<ValidateWorkflowTransitionRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    let definition = state
        .workflow_definition(&tenant_id(&headers), &module_key, &feature_key)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!("Workflow not found: {module_key}.{feature_key}"))
        })?;
    let transition = definition
        .transitions
        .iter()
        .find(|transition| {
            transition.from == request.current_state && transition.action == request.action
        })
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "Transition {} from {} is not valid for {}.{} v{}",
                request.action,
                request.current_state,
                definition.module,
                definition.feature,
                definition.version
            ))
        })?;
    require_effective_permission(&access, &transition.required_permission)?;
    Ok(Json(ApiResponse::new(json!({
        "allowed": true,
        "from": transition.from,
        "to": transition.to,
        "action": transition.action,
        "requiredPermission": transition.required_permission,
        "requiredRole": transition.required_role,
        "workflowVersion": definition.version,
    }))))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let identity = state
        .authenticate_credentials(&request.email, &request.password, None)
        .await?;
    let Some(identity) = identity else {
        return Err(ApiError::InvalidCredentials);
    };
    let session = state.create_session(identity).await?;
    Ok(login_response(
        session,
        request.session_mode == SessionMode::Token,
    ))
}

/// Public reset request. Always answers 202 with the same body so the endpoint cannot
/// be used to discover which email addresses have accounts.
async fn forgot_password(
    State(state): State<AppState>,
    Json(request): Json<ForgotPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    state
        .request_password_reset(&request.email, &public_base_url())
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ApiResponse::new(json!({
            "message": "If that email has an account, a reset link is on its way."
        }))),
    ))
}

/// Public reset completion. Consumes a one-time token and signs out every session.
async fn reset_password(
    State(state): State<AppState>,
    Json(request): Json<ResetPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    let token = request.token.trim();
    if token.is_empty() {
        return Err(ApiError::BadRequest("A reset token is required".into()));
    }
    if request.password.chars().count() < MINIMUM_PASSWORD_LENGTH {
        return Err(ApiError::BadRequest(format!(
            "Password must be at least {MINIMUM_PASSWORD_LENGTH} characters"
        )));
    }
    if state.reset_password(token, &request.password).await? {
        Ok((
            StatusCode::OK,
            Json(ApiResponse::new(json!({
                "message": "Your password has been updated. Sign in with your new password."
            }))),
        ))
    } else {
        Err(ApiError::BadRequest(
            "This reset link is invalid or has expired. Request a new one.".into(),
        ))
    }
}

/// Origin used to build reset links. Must match where the frontend is served.
pub(crate) fn public_base_url() -> String {
    std::env::var("APP_PUBLIC_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "http://localhost:3000".into())
}

async fn me(
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<SessionData>>> {
    Ok(Json(ApiResponse::new(SessionData {
        student: principal.student,
        session_id: principal.session_id,
        roles: principal.roles,
    })))
}

async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Option<Json<RefreshRequest>>,
) -> ApiResult<impl IntoResponse> {
    let body_token = request
        .and_then(|Json(request)| request.refresh_token)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let token_mode = body_token.is_some();
    if !token_mode {
        enforce_cookie_request_origin(&headers)?;
    }
    let token = body_token
        .or_else(|| cookie_value(&headers, "sc_session"))
        .ok_or(ApiError::InvalidRefreshToken)?;
    match state.refresh_session(&token).await? {
        RefreshSessionResult::Rotated(session) => Ok(login_response(*session, token_mode)),
        RefreshSessionResult::ConcurrentRefresh => Err(ApiError::Conflict(
            "A refresh is already in progress; use the token returned by the first request".into(),
        )),
        RefreshSessionResult::Invalid => Err(ApiError::InvalidRefreshToken),
        RefreshSessionResult::ReuseDetected => Err(ApiError::RefreshTokenReuse),
    }
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Option<Json<LogoutRequest>>,
) -> ApiResult<impl IntoResponse> {
    let body_token = request
        .and_then(|Json(request)| request.refresh_token)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if body_token.is_none() && bearer_token(&headers).is_none() {
        enforce_cookie_request_origin(&headers)?;
    }
    if let Some(token) = access_token(&headers) {
        match state.authenticate_access_token(&token).await? {
            AccessTokenAuthentication::Authenticated(principal) => {
                state.revoke_session(principal.session_id).await?;
            }
            AccessTokenAuthentication::Expired
            | AccessTokenAuthentication::Invalid
            | AccessTokenAuthentication::SessionInactive => {}
        }
    }
    if let Some(token) = body_token.or_else(|| cookie_value(&headers, "sc_session")) {
        state.revoke_refresh_token(&token).await?;
    }
    let response_headers = cleared_auth_cookies();
    Ok((response_headers, StatusCode::NO_CONTENT))
}

async fn get_app_state(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let document = state
        .app_state(&principal.student.tenant_id, &principal.student.id)
        .await?;
    Ok(Json(ApiResponse::new(json!(document))))
}

async fn save_app_state(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Json(request): Json<SaveAppStateRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if let Some(action) = request.action.as_deref() {
        tracing::info!(student_id = %principal.student.id, %action, "local app state updated");
    }
    let document = state
        .save_app_state(
            principal.student.tenant_id,
            principal.student.id,
            request.state,
        )
        .await?;
    Ok(Json(ApiResponse::new(json!(document))))
}

/// Navigation the caller is allowed to see, recomputed from live grants.
async fn get_navigation(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let navigation = state
        .navigation(&principal.student.tenant_id, &access)
        .await?;
    Ok(Json(ApiResponse::new(navigation)))
}

/// Mints a short-lived token for the realtime WebSocket.
///
/// Authenticated with the normal session, so it travels safely through the Next.js
/// proxy. The returned token lives for one minute: long enough to open a socket,
/// short enough to bound its exposure in a URL.
async fn realtime_token(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let issued = state.issue_realtime_token(&principal)?;
    Ok(Json(ApiResponse::new(json!({
        "token": issued.token,
        "expiresAt": issued.expires_at,
    }))))
}

async fn get_effective_dashboard(
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let widgets = crate::dashboard::effective_widgets(None, &access);
    Ok(Json(ApiResponse::new(json!({ "widgets": widgets }))))
}

fn ensure_module(state: &AppState, module_key: &str) -> ApiResult<crate::models::ModuleDescriptor> {
    state
        .module(module_key)
        .ok_or_else(|| ApiError::NotFound(format!("Unknown module: {module_key}")))
}

fn require_module_record_permission(
    access: &EffectiveAccess,
    module_key: &str,
    action: &str,
) -> ApiResult<()> {
    require_effective_permission(access, &format!("{module_key}.records.{action}"))
}

fn can_access_module(access: &EffectiveAccess, module_key: &str) -> bool {
    access.allows("*")
        || access
            .permissions
            .iter()
            .any(|permission| permission.starts_with(&format!("{module_key}.")))
}

fn module_read_permission(access: &EffectiveAccess, module_key: &str) -> Option<String> {
    if access.allows("*") {
        return Some("*".into());
    }
    access
        .permissions
        .iter()
        .find(|permission| {
            permission.starts_with(&format!("{module_key}."))
                && (permission.ends_with(".read") || permission.ends_with(".read_submissions"))
        })
        .cloned()
}

fn tenant_id(headers: &HeaderMap) -> String {
    headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tenant-local")
        .to_owned()
}

fn cookie_value(headers: &HeaderMap, expected_name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == expected_name).then(|| value.to_owned())
            })
        })
}

fn access_token(headers: &HeaderMap) -> Option<String> {
    bearer_token(headers).or_else(|| cookie_value(headers, "sc_access"))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("Bearer") && !token.contains(' '))
        .map(|(_, token)| token)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn enforce_cookie_request_origin(headers: &HeaderMap) -> ApiResult<()> {
    if !matches!(
        std::env::var("APP_ENV").as_deref(),
        Ok("production") | Ok("staging")
    ) {
        return Ok(());
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Forbidden)?;
    let allowed = std::env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|value| value == origin);
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

pub async fn authorize_request(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> ApiResult<Response> {
    // WebSocket clients cannot reliably control cookies or attach a bearer header.
    // Authenticate the one-time realtime ticket in the endpoint so a stale access
    // cookie cannot take precedence over the fresh ticket during the upgrade.
    if is_realtime_websocket(request.uri().path()) {
        return Ok(next.run(request).await);
    }
    if !requires_authorization(request.method(), request.uri().path()) {
        return Ok(next.run(request).await);
    }
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) && bearer_token(request.headers()).is_none()
        && cookie_value(request.headers(), "sc_access").is_some()
    {
        enforce_cookie_request_origin(request.headers())?;
    }
    let token = access_token(request.headers())
        .or_else(|| {
            is_realtime_stream(request.uri().path())
                .then(|| realtime_query_token(request.uri()))
                .flatten()
        })
        .ok_or(ApiError::Unauthorized)?;
    let mut principal = match state.authenticate_access_token(&token).await? {
        AccessTokenAuthentication::Authenticated(principal) => *principal,
        AccessTokenAuthentication::Expired => return Err(ApiError::AccessTokenExpired),
        AccessTokenAuthentication::Invalid => return Err(ApiError::InvalidAccessToken),
        AccessTokenAuthentication::SessionInactive => return Err(ApiError::SessionInactive),
    };
    if let Some(requested_tenant) = request
        .headers()
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        && requested_tenant != principal.student.tenant_id
    {
        return Err(ApiError::Forbidden);
    }
    let surface = request
        .headers()
        .get("x-client-surface")
        .and_then(|value| value.to_str().ok())
        .filter(|value| matches!(*value, "app" | "website"))
        .unwrap_or("app");
    let access = state
        .effective_access_for_surface(&principal.student.tenant_id, &principal.student.id, surface)
        .await?;
    principal.roles = access.roles.clone();
    principal.student.role = access.roles.first().cloned().unwrap_or_default();
    principal.student.access = access.permissions.clone();
    request.headers_mut().insert(
        HeaderName::from_static("x-tenant-id"),
        HeaderValue::from_str(&principal.student.tenant_id).map_err(|_| ApiError::Internal)?,
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-user-id"),
        HeaderValue::from_str(&principal.student.id).map_err(|_| ApiError::Internal)?,
    );
    let role = principal.roles.first().map_or("unassigned", String::as_str);
    request.headers_mut().insert(
        HeaderName::from_static("x-user-role"),
        HeaderValue::from_str(role).map_err(|_| ApiError::Internal)?,
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-user-roles"),
        HeaderValue::from_str(
            &serde_json::to_string(&access.roles).map_err(|_| ApiError::Internal)?,
        )
        .map_err(|_| ApiError::Internal)?,
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-user-permissions"),
        HeaderValue::from_str(
            &serde_json::to_string(&access.permissions).map_err(|_| ApiError::Internal)?,
        )
        .map_err(|_| ApiError::Internal)?,
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-permission-scopes"),
        HeaderValue::from_str(
            &serde_json::to_string(&access.scopes).map_err(|_| ApiError::Internal)?,
        )
        .map_err(|_| ApiError::Internal)?,
    );
    request.extensions_mut().insert(access);
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

fn requires_authorization(method: &Method, path: &str) -> bool {
    // A guardian has no account: the one-time token in the path is the whole
    // credential, and it authorises exactly one decision on one request.
    let is_public_guardian_route = path.starts_with("/api/v1/public/gatepass/approvals/")
        && (*method == Method::GET || *method == Method::POST);

    let is_public_crm_route = (*method == Method::GET && path == "/api/v1/crm/health")
        || (path.starts_with("/api/v1/crm/public/applications/")
            && (*method == Method::GET || *method == Method::POST))
        || (*method == Method::POST && path.starts_with("/api/media/public-application/"))
        || (*method == Method::POST
            && path.starts_with("/api/v1/crm/public/forms/")
            && path.ends_with("/submit"));

    !is_public_crm_route
        && !is_public_guardian_route
        && (path == "/api/state"
            || path == "/api/media/upload"
            || path == "/api/v1"
            || path.starts_with("/api/v1/")
            || path == "/api/auth/me"
            || path == "/api/auth/realtime-token")
}

/// The realtime stream is the one route that cannot present a bearer header.
///
/// Browsers cannot set headers on a WebSocket handshake, and Next.js does not proxy
/// upgrade requests, so the socket connects straight to this API from a different
/// origin than the one holding the session cookie. It therefore carries a short-lived
/// token minted by `POST /api/auth/realtime-token` in the query string instead.
fn is_realtime_stream(path: &str) -> bool {
    path == "/api/v1/crm/events"
}

fn is_realtime_websocket(path: &str) -> bool {
    path == "/api/v1/realtime/ws"
}

/// Reads the realtime token from the query string of a WebSocket handshake.
fn realtime_query_token(uri: &axum::http::Uri) -> Option<String> {
    let query = uri.query()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "access_token").then(|| percent_decode(value))
    })
}

/// Minimal percent-decoding for a JWT, which only ever needs `%2E`-style escapes.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            out.push(byte as char);
            index += 3;
            continue;
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

fn login_response(
    session: CreatedAuthSession,
    expose_refresh_token: bool,
) -> (HeaderMap, Json<ApiResponse<LoginData>>) {
    let mut headers = HeaderMap::new();
    if !expose_refresh_token {
        let secure = secure_cookie_suffix();
        let access_max_age = (session.access_expires_at - Utc::now())
            .num_seconds()
            .max(1);
        let refresh_max_age = (session.refresh_expires_at - Utc::now())
            .num_seconds()
            .max(1);
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "sc_access={}; HttpOnly; SameSite=Lax; Path=/; Max-Age={access_max_age}{secure}",
                session.access_token
            ))
            .expect("generated access cookie is valid"),
        );
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "sc_session={}; HttpOnly; SameSite=Lax; Path=/api/auth; Max-Age={refresh_max_age}{secure}",
                session.refresh_token
            ))
            .expect("generated refresh cookie is valid"),
        );
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let data = LoginData {
        student: session.student,
        access_token: expose_refresh_token.then_some(session.access_token),
        token_type: "Bearer",
        expires_at: session.access_expires_at,
        session_id: session.session_id,
        roles: session.roles,
        refresh_token: expose_refresh_token.then_some(session.refresh_token),
    };
    (headers, Json(ApiResponse::new(data)))
}

fn cleared_auth_cookies() -> HeaderMap {
    let mut headers = HeaderMap::new();
    let secure = secure_cookie_suffix();
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "sc_access=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure}"
        ))
        .expect("cleared access cookie is valid"),
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "sc_session=; HttpOnly; SameSite=Lax; Path=/api/auth; Max-Age=0{secure}"
        ))
        .expect("cleared refresh cookie is valid"),
    );
    headers
}

fn secure_cookie_suffix() -> &'static str {
    match std::env::var("APP_ENV").as_deref() {
        Ok("production") | Ok("staging") => "; Secure",
        _ => "",
    }
}

fn valid_permission_scope(scope: &str) -> bool {
    matches!(
        scope,
        "own" | "assigned" | "department" | "institution" | "all"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn access(permissions: &[&str]) -> EffectiveAccess {
        EffectiveAccess {
            roles: vec!["test_role".into()],
            portal_families: vec!["staff".into()],
            permissions: permissions.iter().map(|value| (*value).into()).collect(),
            scopes: HashMap::new(),
        }
    }

    #[test]
    fn module_visibility_requires_an_effective_module_permission() {
        let crm_reader = access(&["crm.leads.read"]);
        assert!(can_access_module(&crm_reader, "crm"));
        assert!(!can_access_module(&crm_reader, "fees"));
        assert_eq!(
            module_read_permission(&crm_reader, "crm").as_deref(),
            Some("crm.leads.read")
        );
    }

    #[test]
    fn generic_records_require_independent_crud_permissions() {
        let reader = access(&["admissions.records.read"]);
        assert!(require_module_record_permission(&reader, "admissions", "read").is_ok());
        assert!(matches!(
            require_module_record_permission(&reader, "admissions", "update"),
            Err(ApiError::Forbidden)
        ));
    }

    #[test]
    fn wildcard_access_keeps_tenant_admin_recovery_access() {
        let admin = access(&["*"]);
        assert!(can_access_module(&admin, "crm"));
        assert_eq!(module_read_permission(&admin, "fees").as_deref(), Some("*"));
        assert!(require_module_record_permission(&admin, "fees", "delete").is_ok());
    }

    #[test]
    fn media_upload_is_always_authenticated() {
        assert!(requires_authorization(&Method::POST, "/api/media/upload"));
    }

    #[test]
    fn realtime_websocket_owns_its_ticket_authentication() {
        assert!(is_realtime_websocket("/api/v1/realtime/ws"));
        assert!(!is_realtime_websocket("/api/v1/crm/events"));
        assert!(!is_realtime_websocket("/api/v1/realtime/ws/other"));
    }

    #[test]
    fn permission_scopes_cover_every_organizational_boundary() {
        for scope in ["own", "assigned", "department", "institution", "all"] {
            assert!(valid_permission_scope(scope), "{scope} should be accepted");
        }
        assert!(!valid_permission_scope("platform"));
        assert!(!valid_permission_scope(""));
    }
}
