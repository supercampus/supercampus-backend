//! Request handlers.
//!
//! Protocol errors and domain refusals are deliberately different things: a
//! guard that blocks a transition is a 200 with `ok: false` and a readable
//! reason, because the desk needs to show the operator *why*. 4xx is reserved
//! for authentication, malformed bodies and unknown case ids.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use supercampus_database::TenantDatabaseManager;

use crate::{
    application::{ActorContext, ApplicationDeskService},
    domain::{ActionKind, ActionPayload, AdmissionTrigger},
    infrastructure::postgres::DeskError,
};

#[derive(Clone)]
pub struct DeskApiState {
    pub databases: Option<TenantDatabaseManager>,
}

pub struct DeskHttpError(pub DeskError);

impl From<DeskError> for DeskHttpError {
    fn from(error: DeskError) -> Self {
        Self(error)
    }
}

impl IntoResponse for DeskHttpError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            DeskError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, self.0.to_string()),
            DeskError::NotFound(_) => (StatusCode::NOT_FOUND, self.0.to_string()),
            DeskError::Conflict(_) => (StatusCode::CONFLICT, self.0.to_string()),
            DeskError::Storage(_) => {
                tracing::error!(error = %self.0, "application desk storage failure");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// Authentication and permission failures.
pub struct AuthError(StatusCode, String);

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn required_header(headers: &HeaderMap, name: &str) -> Result<String, AuthError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AuthError(StatusCode::UNAUTHORIZED, format!("{name} is required")))
}

/// The gateway middleware has already authenticated the caller and stamped the
/// tenant, user and effective permissions onto the request.
fn context(headers: &HeaderMap) -> Result<(String, ActorContext), AuthError> {
    let tenant = required_header(headers, "x-tenant-id")?;
    let user_id = required_header(headers, "x-user-id")?;
    let roles: Vec<String> = headers
        .get("x-user-roles")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    let permissions: Vec<String> = headers
        .get("x-user-permissions")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();

    Ok((
        tenant,
        ActorContext {
            user_id,
            roles,
            permissions,
        },
    ))
}

fn require(actor: &ActorContext, permission: &str) -> Result<(), AuthError> {
    if actor.has(permission) {
        return Ok(());
    }
    Err(AuthError(
        StatusCode::FORBIDDEN,
        format!("{permission} is required"),
    ))
}

impl DeskApiState {
    async fn service(&self, tenant: &str) -> Result<ApplicationDeskService, DeskHttpError> {
        let databases = self
            .databases
            .as_ref()
            .ok_or(DeskHttpError(DeskError::Unavailable))?;
        let database = databases
            .tenant(tenant)
            .await
            .map_err(|error| DeskHttpError(DeskError::Storage(error.to_string())))?;
        Ok(ApplicationDeskService::new(database))
    }
}

/// `GET /v1/application-desk/cases`
pub async fn list_cases(
    State(state): State<DeskApiState>,
    headers: HeaderMap,
) -> Result<Response, Response> {
    let (tenant, actor) = context(&headers).map_err(IntoResponse::into_response)?;
    require(&actor, "application-desk.view").map_err(IntoResponse::into_response)?;

    let service = state
        .service(&tenant)
        .await
        .map_err(IntoResponse::into_response)?;
    let snapshot = service
        .snapshot(&tenant)
        .await
        .map_err(|error| DeskHttpError(error).into_response())?;

    Ok(Json(json!({ "data": snapshot.to_json() })).into_response())
}

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: ActionKind,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: Option<ActionPayload>,
}

/// `POST /v1/application-desk/cases/{id}/actions`
pub async fn act_on_case(
    State(state): State<DeskApiState>,
    headers: HeaderMap,
    Path(case_id): Path<String>,
    body: Result<Json<ActionRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Response> {
    let (tenant, actor) = context(&headers).map_err(IntoResponse::into_response)?;
    let Json(request) = body.map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.body_text() })),
        )
            .into_response()
    })?;

    // Each action carries its own permission so verification, approval and
    // activation can be held by different teams.
    require(&actor, request.action.required_permission()).map_err(IntoResponse::into_response)?;

    let service = state
        .service(&tenant)
        .await
        .map_err(IntoResponse::into_response)?;
    let outcome = service
        .act(
            &tenant,
            &actor,
            crate::application::ActionRequest {
                case_id,
                action: request.action,
                reason: request.reason,
                payload: request.payload.unwrap_or_default(),
            },
        )
        .await
        .map_err(|error| DeskHttpError(error).into_response())?;

    Ok(Json(json!({
        "ok": outcome.ok,
        "error": outcome.error,
        "data": outcome.snapshot.to_json(),
    }))
    .into_response())
}

/// `POST /v1/application-desk/cases` — intake.
pub async fn open_case(
    State(state): State<DeskApiState>,
    headers: HeaderMap,
    body: Result<Json<AdmissionTrigger>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Response> {
    let (tenant, actor) = context(&headers).map_err(IntoResponse::into_response)?;
    require(&actor, "application-desk.create").map_err(IntoResponse::into_response)?;

    let Json(trigger) = body.map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.body_text() })),
        )
            .into_response()
    })?;

    let service = state
        .service(&tenant)
        .await
        .map_err(IntoResponse::into_response)?;
    let (created, refusal, snapshot) = service
        .open_case(&tenant, &actor, trigger)
        .await
        .map_err(|error| DeskHttpError(error).into_response())?;

    Ok(Json(json!({
        "ok": created,
        "error": refusal,
        "data": snapshot.to_json(),
    }))
    .into_response())
}

/// `GET /v1/application-desk/health`
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "module": "application-desk" }))
}
