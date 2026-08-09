use axum::{
    Json,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use supercampus_application_desk::{
    application::{ActorContext as DeskActorContext, ApplicationDeskService},
    domain::{AdmissionTrigger, ApplicantSnapshot},
};
use supercampus_database::TenantDatabaseManager;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    api::dto::*,
    application::{ActorContext, CrmService},
    domain::CrmError,
};

#[derive(Clone)]
pub struct CrmApiState {
    pub databases: Option<TenantDatabaseManager>,
    pub catalog_service: CrmService,
    pub realtime_wake: broadcast::Sender<()>,
}

impl CrmApiState {
    async fn service(&self, tenant: &str) -> Result<CrmService, CrmHttpError> {
        let databases = self
            .databases
            .as_ref()
            .ok_or(CrmHttpError(CrmError::Unavailable))?;
        let database = databases
            .tenant(tenant)
            .await
            .map_err(|error| CrmHttpError(CrmError::Storage(error.to_string())))?;
        Ok(CrmService::new(Some(database)))
    }

    async fn reflect_offer_in_application_desk(
        &self,
        context: &RequestContext,
        lead: &crate::domain::Lead,
    ) -> Result<(), CrmHttpError> {
        if lead.stage_key != "offer_status" {
            return Ok(());
        }
        let databases = self
            .databases
            .as_ref()
            .ok_or(CrmHttpError(CrmError::Unavailable))?;
        let database = databases
            .tenant(&context.tenant)
            .await
            .map_err(|error| CrmHttpError(CrmError::Storage(error.to_string())))?;
        let desk = ApplicationDeskService::new(database);
        let actor = DeskActorContext {
            user_id: context.actor.user_id.clone(),
            roles: context.actor.roles.clone(),
            permissions: vec!["application-desk.create".into()],
        };
        let program_id = lead
            .academic
            .get("programId")
            .or_else(|| lead.academic.get("program"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let trigger = AdmissionTrigger {
            applicant_id: lead.id.to_string(),
            application_id: format!("CRM-APP-{}", lead.id),
            admission_id: format!("CRM-OFFER-{}", lead.id),
            crm_lead_id: Some(lead.id),
            admission_status: "CONFIRMED".into(),
            program_id,
            fee_paid: lead.fee_payment_confirmed,
            applicant: ApplicantSnapshot {
                full_name: Some(lead.full_name.clone()),
                email: lead.email.clone(),
                phone: lead.phone.clone(),
                guardian_name: lead.parent_name.clone(),
                guardian_email: None,
            },
            ..AdmissionTrigger::default()
        };
        desk.open_case(&context.tenant, &actor, trigger)
            .await
            .map_err(|error| CrmHttpError(CrmError::Storage(error.to_string())))?;
        Ok(())
    }
}

pub struct RequestContext {
    pub tenant: String,
    pub actor: ActorContext,
}

impl RequestContext {
    pub fn public_from_headers(headers: &HeaderMap) -> Result<Self, CrmHttpError> {
        Ok(Self {
            tenant: required_header(headers, "x-tenant-id")?,
            actor: ActorContext {
                user_id: format!("public-enquiry-{}", Uuid::new_v4()),
                roles: vec!["public".into()],
                permissions: Default::default(),
                permission_scopes: Default::default(),
                public: true,
                ip_address: headers
                    .get("x-forwarded-for")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(',').next())
                    .map(str::trim)
                    .map(str::to_owned),
            },
        })
    }
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, CrmHttpError> {
        let tenant = required_header(headers, "x-tenant-id")?;
        let user_id = required_header(headers, "x-user-id")?;
        let roles = json_header(headers, "x-user-roles")?;
        let permissions = json_header::<Vec<String>>(headers, "x-user-permissions")?
            .into_iter()
            .collect();
        let permission_scopes = json_header(headers, "x-permission-scopes")?;
        let ip_address = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .map(str::to_owned);
        Ok(Self {
            tenant,
            actor: ActorContext {
                user_id,
                roles,
                permissions,
                permission_scopes,
                public: false,
                ip_address,
            },
        })
    }
}

fn json_header<T: DeserializeOwned>(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<T, CrmHttpError> {
    let value = required_header(headers, name)?;
    serde_json::from_str(&value).map_err(|_| CrmHttpError(CrmError::Unauthorized))
}

fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, CrmHttpError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(CrmHttpError(CrmError::Unauthorized))
}

#[derive(Debug)]
pub struct CrmHttpError(pub CrmError);

impl From<CrmError> for CrmHttpError {
    fn from(value: CrmError) -> Self {
        Self(value)
    }
}

impl IntoResponse for CrmHttpError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self.0 {
            CrmError::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "CRM record was not found".to_owned(),
            ),
            CrmError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication context is required".to_owned(),
            ),
            CrmError::Forbidden(message) => (StatusCode::FORBIDDEN, "forbidden", message),
            CrmError::Validation(message) => (StatusCode::BAD_REQUEST, "validation_error", message),
            CrmError::Conflict(message) => (StatusCode::CONFLICT, "conflict", message),
            CrmError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "database_unavailable",
                "CRM database is not configured".to_owned(),
            ),
            CrmError::Storage(message) => {
                tracing::error!(error = %message, "CRM storage request failed");
                let client_message = if cfg!(debug_assertions) {
                    format!("CRM storage request failed: {message}")
                } else {
                    "CRM storage request failed".to_owned()
                };
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    client_message,
                )
            }
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

fn ok<T: Serialize>(data: T) -> Json<ApiResponse<T>> {
    Json(ApiResponse::new(data))
}

pub async fn health() -> Json<Value> {
    Json(json!({ "module": "crm", "status": "ok", "contract": "v1" }))
}

pub async fn roles(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service.roles(&context.tenant, &context.actor).await?))
}

pub async fn effective_permissions(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    Ok(ok(state
        .catalog_service
        .effective_permissions(&context.actor)))
}

pub async fn create_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateLeadRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let lead = service
        .create_lead(&context.tenant, &context.actor, request)
        .await?;
    Ok((StatusCode::CREATED, ok(lead)))
}

pub async fn import_leads(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<BulkImportLeadsRequest>,
) -> Result<Json<ApiResponse<BulkImportLeadsResponse>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .import_leads(&context.tenant, &context.actor, request)
        .await?))
}

pub async fn list_leads(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Query(filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .list_leads(&context.tenant, &context.actor, &filters)
        .await?))
}

pub async fn list_unassigned_leads(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Query(filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .unassigned_leads(&context.tenant, &context.actor, filters)
        .await?))
}

pub async fn get_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .get_lead(&context.tenant, &context.actor, id)
        .await?))
}

pub async fn update_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateLeadRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .update_lead(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn delete_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    service
        .delete_lead(&context.tenant, &context.actor, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn assign_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<AssignLeadRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .assign(&context.tenant, &context.actor, id, request, false)
        .await?))
}

pub async fn claim_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ClaimLeadRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .claim(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn reassign_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<AssignLeadRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .assign(&context.tenant, &context.actor, id, request, true)
        .await?))
}

pub async fn move_stage(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<MoveStageRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let moved = service
        .move_stage(&context.tenant, &context.actor, id, request)
        .await?;
    state
        .reflect_offer_in_application_desk(&context, &moved)
        .await?;
    let _ = state.realtime_wake.send(());
    Ok(ok(moved))
}

pub async fn mark_prospect(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<IntakeStatusRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .prospect_or_defer(&context.tenant, &context.actor, id, "prospect", request)
        .await?))
}

pub async fn mark_deferred(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<IntakeStatusRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .prospect_or_defer(&context.tenant, &context.actor, id, "deferred", request)
        .await?))
}

pub async fn hold(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<HoldRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .hold(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn release_hold(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ReasonRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .release_hold(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn archive(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .archive(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn unarchive(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<UnarchiveRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .unarchive(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn timeline(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .timeline(&context.tenant, &context.actor, id)
        .await?))
}

pub async fn board(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Query(filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .board(&context.tenant, &context.actor, filters)
        .await?))
}

pub async fn add_lead_note(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<CreateLeadNoteRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .add_lead_note(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn add_lead_task(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<CreateLeadTaskRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .add_lead_task(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn request_stage_move(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<MoveStageRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .request_stage_move(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn list_move_requests(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .move_requests(&context.tenant, &context.actor)
        .await?))
}

pub async fn approve_move_request(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<MoveRequestDecision>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let decision = service
        .decide_stage_move(&context.tenant, &context.actor, id, true, request)
        .await?;
    if decision.status == "approved" && decision.to_stage == "offer_status" {
        let moved = service
            .get_lead(&context.tenant, &context.actor, decision.lead_id)
            .await?;
        state
            .reflect_offer_in_application_desk(&context, &moved)
            .await?;
    }
    let _ = state.realtime_wake.send(());
    Ok(ok(decision))
}

pub async fn reject_move_request(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<MoveRequestDecision>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .decide_stage_move(&context.tenant, &context.actor, id, false, request)
        .await?))
}

pub async fn application_link(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .application_link(&context.tenant, &context.actor, id)
        .await?))
}

pub async fn my_board(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Query(filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .my_board(&context.tenant, &context.actor, filters)
        .await?))
}

pub async fn operations_dashboard(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .operations_dashboard(&context.tenant, &context.actor)
        .await?))
}

pub async fn recent_activity(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Vec<Value>>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .recent_activity(&context.tenant, &context.actor)
        .await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct EventCursor {
    #[serde(default)]
    cursor: i64,
}

pub async fn realtime_events(
    ws: WebSocketUpgrade,
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Query(query): Query<EventCursor>,
) -> Result<Response, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let initial = service
        .realtime_events(&context.tenant, &context.actor, query.cursor)
        .await?;
    let wake = state.realtime_wake.subscribe();
    Ok(ws.on_upgrade(move |socket| {
        stream_realtime_events(socket, service, context.tenant, query.cursor, initial, wake)
    }))
}

async fn stream_realtime_events(
    mut socket: WebSocket,
    service: CrmService,
    tenant: String,
    mut cursor: i64,
    mut events: Vec<Value>,
    mut wake: broadcast::Receiver<()>,
) {
    loop {
        for event in events {
            if let Some(next_cursor) = event.get("cursor").and_then(Value::as_i64) {
                cursor = next_cursor;
            }
            if socket
                .send(Message::Text(event.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }

        // Wait for the next poll tick, but abandon the stream the moment the client
        // goes away. Watching only for send failures is not enough: a quiet tenant
        // produces no sends, so a disconnected socket would otherwise keep polling the
        // database once a second for the lifetime of the process.
        let client_disconnected = tokio::select! {
            incoming = socket.recv() => matches!(
                incoming,
                // Close frame, transport error, or end of stream: the client is gone.
                None | Some(Err(_)) | Some(Ok(Message::Close(_)))
            ),
            // Movement handlers wake every connected tenant stream immediately.
            // The heartbeat preserves outbox recovery for events written by jobs or
            // another process that cannot access this in-memory notifier.
            _ = wake.recv() => false,
            _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => false,
        };
        if client_disconnected {
            return;
        }

        events = match service.realtime_events_raw(&tenant, cursor).await {
            Ok(events) => events,
            Err(error) => {
                let payload = json!({
                    "eventType": "crm.stream_error",
                    "payload": { "message": error.to_string() }
                });
                let _ = socket.send(Message::Text(payload.to_string().into())).await;
                return;
            }
        };
    }
}
pub async fn stages(State(state): State<CrmApiState>) -> Json<ApiResponse<Value>> {
    ok(state.catalog_service.stage_catalog())
}

pub async fn stage_leads(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(stage): Path<String>,
    Query(mut filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    filters.stage = Some(stage);
    Ok(ok(service
        .list_leads(&context.tenant, &context.actor, &filters)
        .await?))
}

pub async fn stage_count(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(stage): Path<String>,
    Query(mut filters): Query<LeadFilters>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    filters.stage = Some(stage.clone());
    let leads = service
        .list_leads(&context.tenant, &context.actor, &filters)
        .await?;
    Ok(ok(json!({ "stage": stage, "count": leads.len() })))
}

pub async fn create_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateFormRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok((
        StatusCode::CREATED,
        ok(service
            .create_form(&context.tenant, &context.actor, request)
            .await?),
    ))
}

pub async fn list_forms(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .list_forms(&context.tenant, &context.actor)
        .await?))
}

pub async fn published_lead_capture_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .published_lead_capture_form(&context.tenant, &context.actor)
        .await?))
}

/// Every published form, so the workspace can discover what an administrator offers.
pub async fn published_forms(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .published_forms(&context.tenant, &context.actor)
        .await?))
}

/// The published form of one type, for example `application` or `document_checklist`.
pub async fn published_form_by_type(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(form_type): Path<String>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .published_form_by_type(&context.tenant, &context.actor, &form_type)
        .await?))
}

pub async fn get_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .get_form(&context.tenant, &context.actor, id)
        .await?))
}

pub async fn update_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateFormRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .update_form(&context.tenant, &context.actor, id, request)
        .await?))
}

pub async fn delete_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    service
        .delete_form(&context.tenant, &context.actor, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn publish_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .set_form_status(&context.tenant, &context.actor, id, "published")
        .await?))
}

pub async fn unpublish_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .set_form_status(&context.tenant, &context.actor, id, "draft")
        .await?))
}

pub async fn submit_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SubmitFormRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok((
        StatusCode::CREATED,
        ok(service
            .submit_form(&context.tenant, &context.actor, id, request)
            .await?),
    ))
}

pub async fn submit_public_form(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SubmitFormRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::public_from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok((
        StatusCode::CREATED,
        ok(service
            .submit_form(&context.tenant, &context.actor, id, request)
            .await?),
    ))
}
pub async fn form_submissions(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .list_submissions(&context.tenant, &context.actor, id)
        .await?))
}

async fn communicate(
    state: CrmApiState,
    headers: HeaderMap,
    channel: &str,
    request: SendCommunicationRequest,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .send_communication(&context.tenant, &context.actor, channel, request)
        .await?))
}

pub async fn send_whatsapp(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<SendCommunicationRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    communicate(state, headers, "whatsapp", request).await
}
pub async fn send_email(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<SendCommunicationRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    communicate(state, headers, "email", request).await
}
pub async fn log_call(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<SendCommunicationRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    communicate(state, headers, "call", request).await
}

pub async fn list_templates(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .list_templates(&context.tenant, &context.actor)
        .await?))
}
pub async fn create_template(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateTemplateRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok((
        StatusCode::CREATED,
        ok(service
            .create_template(&context.tenant, &context.actor, request)
            .await?),
    ))
}

pub async fn list_counselors(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .counselors(&context.tenant, &context.actor)
        .await?))
}
pub async fn upsert_counselor(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CounselorCapacityRequest>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .upsert_counselor(&context.tenant, &context.actor, request)
        .await?))
}

pub async fn list_campaigns(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .campaigns(&context.tenant, &context.actor)
        .await?))
}

pub async fn upsert_campaign(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateCampaignRequest>,
) -> Result<impl IntoResponse, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok((
        StatusCode::CREATED,
        ok(service
            .upsert_campaign(&context.tenant, &context.actor, request)
            .await?),
    ))
}

pub async fn configuration(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .configuration(&context.tenant, &context.actor)
        .await?))
}
pub async fn workflow_toggle(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<WorkflowToggleRequest>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .upsert_workflow_toggle(&context.tenant, &context.actor, request)
        .await?))
}
pub async fn automation_toggle(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<AutomationToggleRequest>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .upsert_automation_toggle(&context.tenant, &context.actor, request)
        .await?))
}
