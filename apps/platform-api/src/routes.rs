use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::{
        ApiResponse, BootstrapDocument, CreateRecordRequest, HealthDocument, LoginData,
        LoginRequest, NavigationItem, PutConfigurationRequest, SaveAppStateRequest,
        UpdateRecordRequest,
    },
    state::AppState,
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
            "/configuration/{namespace}",
            get(get_configuration).put(put_configuration),
        )
        .route(
            "/{module_key}/records",
            get(list_records).post(create_record),
        )
        .route(
            "/{module_key}/records/{record_id}",
            get(get_record).patch(update_record).delete(delete_record),
        );

    let api = Router::new()
        .route("/auth/tenants", get(list_tenants))
        .route("/auth/login", post(login))
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/state", get(get_app_state).put(save_app_state))
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
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<BootstrapDocument>>> {
    let tenant_id = tenant_id(&headers);
    let user_id = match session_token(&headers) {
        Some(token) => state
            .session(&token)
            .await?
            .map_or_else(|| "anonymous-local".into(), |student| student.id),
        None => "anonymous-local".into(),
    };
    let modules = state.modules();
    let navigation = modules
        .iter()
        .map(|module| NavigationItem {
            id: module.key.clone(),
            label: module.name.clone(),
            route: format!("/dashboard/{}", module.key),
            required_permission: format!("{}.read", module.key),
        })
        .collect();
    Ok(Json(ApiResponse::new(BootstrapDocument {
        tenant_id,
        user_id,
        services: state.services(),
        modules,
        navigation,
    })))
}

async fn list_modules(State(state): State<AppState>) -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(json!(state.modules())))
}

async fn get_module(
    State(state): State<AppState>,
    Path(module_key): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let module = ensure_module(&state, &module_key)?;
    Ok(Json(ApiResponse::new(json!(module))))
}

async fn list_records(
    State(state): State<AppState>,
    Path(module_key): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    let records = state
        .list_records(&tenant_id(&headers), &module_key)
        .await?;
    Ok(Json(ApiResponse::new(json!(records))))
}

async fn create_record(
    State(state): State<AppState>,
    Path(module_key): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateRecordRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    ensure_module(&state, &module_key)?;
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
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    let record = state
        .record(&tenant_id(&headers), &module_key, record_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Record not found: {record_id}")))?;
    Ok(Json(ApiResponse::new(json!(record))))
}

async fn update_record(
    State(state): State<AppState>,
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<UpdateRecordRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    ensure_module(&state, &module_key)?;
    let record = state
        .update_record(&tenant_id(&headers), &module_key, record_id, request.data)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Record not found: {record_id}")))?;
    Ok(Json(ApiResponse::new(json!(record))))
}

async fn delete_record(
    State(state): State<AppState>,
    Path((module_key, record_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    ensure_module(&state, &module_key)?;
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
    Path(namespace): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let document = state
        .configuration(&tenant_id(&headers), &namespace)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Configuration not found: {namespace}")))?;
    Ok(Json(ApiResponse::new(json!(document))))
}

async fn put_configuration(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PutConfigurationRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if namespace.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "Configuration namespace is required".into(),
        ));
    }
    let document = state
        .put_configuration(tenant_id(&headers), namespace, request.value)
        .await?;
    Ok(Json(ApiResponse::new(json!(document))))
}

async fn list_tenants() -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(json!([{
        "id": "tenant-local",
        "code": "LOCAL",
        "name": "SuperCampus Local",
        "city": "Local Development"
    }])))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let expected_password =
        std::env::var("DEV_LOGIN_PASSWORD").unwrap_or_else(|_| "SuperCampus@123".into());
    if !request.email.contains('@') || request.password != expected_password {
        return Err(ApiError::Unauthorized);
    }
    let (token, student) = state.create_session(request.email).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "sc_session={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=28800"
        ))
        .expect("generated session cookie is valid"),
    );
    Ok((headers, Json(ApiResponse::new(LoginData { student }))))
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<LoginData>>> {
    let student = authenticated_student(&state, &headers).await?;
    Ok(Json(ApiResponse::new(LoginData { student })))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    if let Some(token) = session_token(&headers) {
        state.remove_session(&token).await?;
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_static("sc_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
    );
    Ok((response_headers, StatusCode::NO_CONTENT))
}

async fn get_app_state(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let student = authenticated_student(&state, &headers).await?;
    let document = state.app_state(&student.id).await?;
    Ok(Json(ApiResponse::new(json!(document))))
}

async fn save_app_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SaveAppStateRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let student = authenticated_student(&state, &headers).await?;
    if let Some(action) = request.action.as_deref() {
        tracing::info!(student_id = %student.id, %action, "local app state updated");
    }
    let document = state.save_app_state(student.id, request.state).await?;
    Ok(Json(ApiResponse::new(json!(document))))
}

fn ensure_module(state: &AppState, module_key: &str) -> ApiResult<crate::models::ModuleDescriptor> {
    state
        .module(module_key)
        .ok_or_else(|| ApiError::NotFound(format!("Unknown module: {module_key}")))
}

fn tenant_id(headers: &HeaderMap) -> String {
    headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tenant-local")
        .to_owned()
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == "sc_session").then(|| value.to_owned())
            })
        })
}

async fn authenticated_student(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<crate::models::AuthStudent> {
    let token = session_token(headers).ok_or(ApiError::Unauthorized)?;
    state.session(&token).await?.ok_or(ApiError::Unauthorized)
}
