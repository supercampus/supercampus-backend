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
use std::time::Duration;
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

    async fn reflect_application_in_desk(
        &self,
        context: &RequestContext,
        lead: &crate::domain::Lead,
        handoff: ApplicationDeskHandoff,
    ) -> Result<(), CrmHttpError> {
        let databases = self
            .databases
            .as_ref()
            .ok_or(CrmHttpError(CrmError::Unavailable))?;
        let database = databases
            .tenant(&context.tenant)
            .await
            .map_err(|error| CrmHttpError(CrmError::Storage(error.to_string())))?;
        let crm = CrmService::new(Some(database.clone()));
        let submitted_application = crm
            .latest_application_submission(&context.tenant, lead.id)
            .await?;
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
        let application_id = format!("CRM-APP-{}", lead.id);
        let admission_id = format!("CRM-ADMISSION-{}", lead.id);
        let (admission_status, handoff_reason) = match handoff {
            ApplicationDeskHandoff::OfferAccepted => ("CONFIRMED", "offer_accepted"),
        };
        let mut attributes = json!({
            "source": lead.source,
            "sourceDetail": lead.source_detail,
            "whatsapp": lead.whatsapp,
            "parentPhone": lead.parent_phone,
            "interest": lead.interest,
            "priority": lead.priority,
            "leadOwner": lead.assigned_to,
            "preferredChannel": lead.preferred_channel,
            "customFields": lead.custom_fields,
            "crmStage": lead.stage_key,
            "crmSubstate": lead.substate_key,
            "leadCreatedAt": lead.created_at,
            "handoffReason": handoff_reason,
            "sourceReferences": {
                "leadId": lead.id,
                "applicationId": application_id,
                "admissionId": admission_id,
            },
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        if let Some(application) = submitted_application {
            attributes.insert("applicationForm".into(), application.clone());
            attributes.insert("applicationFormHistory".into(), json!([application]));
            attributes.insert("applicationFormRequired".into(), json!(true));
        }
        let trigger = AdmissionTrigger {
            applicant_id: lead.id.to_string(),
            application_id: application_id.clone(),
            admission_id: admission_id.clone(),
            crm_lead_id: Some(lead.id),
            admission_status: admission_status.into(),
            program_id,
            fee_paid: lead.fee_payment_confirmed,
            applicant: ApplicantSnapshot {
                full_name: Some(lead.full_name.clone()),
                email: lead.email.clone(),
                phone: lead.phone.clone(),
                guardian_name: lead.parent_name.clone(),
                guardian_email: None,
            },
            attributes,
            ..AdmissionTrigger::default()
        };
        desk.open_case(&context.tenant, &actor, trigger)
            .await
            .map_err(|error| CrmHttpError(CrmError::Storage(error.to_string())))?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplicationDeskHandoff {
    OfferAccepted,
}

fn application_desk_handoff(
    to_stage: &str,
    to_substate: Option<&str>,
) -> Option<ApplicationDeskHandoff> {
    match (to_stage, to_substate) {
        ("offer_status", Some("accepted")) => Some(ApplicationDeskHandoff::OfferAccepted),
        _ => None,
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
            CrmError::ExternalService(message) => {
                tracing::error!(error = %message, "CRM AI request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "ai_service_unavailable",
                    "The AI assistant is temporarily unavailable".to_owned(),
                )
            }
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrmTextAssistantRequest {
    pub input: String,
    #[serde(default = "default_assistant_intent")]
    pub intent: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrmTextAssistantResponse {
    pub content: String,
    pub intent: String,
    pub model: String,
    pub grounded: bool,
    pub action: Option<CrmAssistantActionProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrmAssistantActionProposal {
    pub action_type: String,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub description: String,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrmAssistantActionRequest {
    pub action: CrmAssistantActionProposal,
}

#[derive(Debug, Deserialize)]
struct AiPlannerResponse {
    #[serde(default)]
    answer: String,
    #[serde(default)]
    action: Option<AiPlannedAction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiPlannedAction {
    #[serde(rename = "type")]
    action_type: String,
    lead_query: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Serialize)]
struct AiChatRequest<'a> {
    model: &'a str,
    messages: Vec<AiChatRequestMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Debug, Serialize)]
struct AiChatRequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct AiChatResponse {
    choices: Vec<AiChatChoice>,
}

#[derive(Debug, Deserialize)]
struct AiChatChoice {
    message: AiChatMessage,
}

#[derive(Debug, Deserialize)]
struct AiChatMessage {
    content: String,
}

fn default_assistant_intent() -> String {
    "general".into()
}

fn ai_environment(primary: &str, legacy: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var(legacy)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

fn ai_chat_endpoint(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/v1") {
        format!("{base_url}/chat/completions")
    } else {
        format!("{base_url}/v1/chat/completions")
    }
}

fn assistant_instruction(intent: &str) -> Option<&'static str> {
    match intent {
        "general" => Some("Answer the user's admissions CRM request directly and practically."),
        "summarize" => Some("Summarize the text into key facts, concerns, and next steps."),
        "extract" => Some(
            "Extract lead details into a clear field-value list. Mark missing fields instead of inventing values.",
        ),
        "follow_up" => Some(
            "Draft a concise, warm follow-up message suitable for an admissions counselor. Do not claim actions or approvals that have not happened.",
        ),
        "next_actions" => Some(
            "Recommend a prioritized counselor action list with short reasons and any information that must be verified.",
        ),
        _ => None,
    }
}

fn pipeline_summary(board: &Value) -> Value {
    let stages = board["stages"]
        .as_array()
        .map(|stages| {
            stages
                .iter()
                .map(|stage| {
                    let mut substate_counts = serde_json::Map::new();
                    for lead in stage["leads"].as_array().into_iter().flatten() {
                        if let Some(substate) = lead["substate_key"].as_str() {
                            let count = substate_counts
                                .get(substate)
                                .and_then(Value::as_u64)
                                .unwrap_or_default()
                                + 1;
                            substate_counts.insert(substate.to_owned(), json!(count));
                        }
                    }
                    json!({
                        "key": stage["key"],
                        "count": stage["count"],
                        "substates": substate_counts,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "source": "live SuperCampus CRM",
        "scope": board["scope"],
        "total": board["total"],
        "stages": stages,
    })
}

fn parse_planner_response(content: &str) -> Option<AiPlannerResponse> {
    let trimmed = content.trim();
    let json_text = if trimmed.starts_with("```") {
        trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))?
            .strip_suffix("```")?
            .trim()
    } else {
        trimmed
    };
    serde_json::from_str(json_text).ok()
}

fn normalize_lead_query(query: &str) -> String {
    let query = query.trim().trim_matches(['"', '\'', '`']);
    let lower = query.to_ascii_lowercase();
    if let Some(index) = lower.find("lead workspace") {
        let name = query[..index]
            .trim()
            .trim_end_matches(['-', '·', ':'])
            .trim();
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    query.to_owned()
}

async fn resolve_action_proposal(
    service: &CrmService,
    context: &RequestContext,
    planned: AiPlannedAction,
) -> Result<Option<CrmAssistantActionProposal>, CrmHttpError> {
    if !context.actor.has("crm.leads.update") {
        return Ok(None);
    }
    if !matches!(
        planned.action_type.as_str(),
        "add_lead_note" | "create_lead_task" | "move_lead"
    ) {
        return Ok(None);
    }
    let normalized_query = normalize_lead_query(&planned.lead_query);
    let query = normalized_query.trim();
    if query.is_empty() {
        return Ok(None);
    }
    let leads = service
        .list_leads(
            &context.tenant,
            &context.actor,
            &LeadFilters {
                search: Some(query.to_owned()),
                limit: Some(5),
                ..LeadFilters::default()
            },
        )
        .await?;
    if leads.len() != 1 {
        return Ok(None);
    }
    let lead = &leads[0];
    let description = match planned.action_type.as_str() {
        "add_lead_note" => format!("Add a note to {}", lead.full_name),
        "create_lead_task" => format!("Create a follow-up task for {}", lead.full_name),
        "move_lead" => format!("Move {} to another pipeline stage", lead.full_name),
        _ => return Ok(None),
    };
    Ok(Some(CrmAssistantActionProposal {
        action_type: planned.action_type,
        lead_id: lead.id,
        lead_name: lead.full_name.clone(),
        description,
        payload: planned.payload,
    }))
}

pub async fn text_assistant(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CrmTextAssistantRequest>,
) -> Result<Json<ApiResponse<CrmTextAssistantResponse>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    if !context
        .actor
        .has_any(&[
            "crm.leads.read",
            "crm.dashboard.read",
            "admissions.read",
            "admissions.records.read",
        ])
    {
        return Err(CrmHttpError(CrmError::Forbidden(
            "Admissions or CRM read access is required".into(),
        )));
    }
    let input = request.input.trim();
    if input.is_empty() {
        return Err(CrmHttpError(CrmError::Validation(
            "text input is required".into(),
        )));
    }
    if input.chars().count() > 12_000 {
        return Err(CrmHttpError(CrmError::Validation(
            "text input is limited to 12000 characters".into(),
        )));
    }
    let intent = request.intent.trim().to_ascii_lowercase();
    let instruction = assistant_instruction(&intent)
        .ok_or_else(|| CrmHttpError(CrmError::Validation("unknown assistant intent".into())))?;
    let service = state.service(&context.tenant).await?;
    let portal_context = if context.actor.has("crm.leads.read") {
        Some(pipeline_summary(
            &service
                .board(&context.tenant, &context.actor, LeadFilters::default())
                .await?,
        ))
    } else {
        None
    };
    let base_url =
        ai_environment("SUPERCAMPUS_AI_BASE_URL", "TIMETABLE_AI_BASE_URL").ok_or_else(|| {
            CrmHttpError(CrmError::ExternalService(
                "AI base URL is not configured".into(),
            ))
        })?;
    let api_key =
        ai_environment("SUPERCAMPUS_AI_API_KEY", "TIMETABLE_AI_API_KEY").ok_or_else(|| {
            CrmHttpError(CrmError::ExternalService(
                "AI API key is not configured".into(),
            ))
        })?;
    let model = ai_environment("SUPERCAMPUS_AI_MODEL", "TIMETABLE_AI_MODEL")
        .unwrap_or_else(|| "Qwen/Qwen2.5-7B-Instruct".into());
    let timeout_seconds = ai_environment(
        "SUPERCAMPUS_AI_TIMEOUT_SECONDS",
        "TIMETABLE_AI_TIMEOUT_SECONDS",
    )
    .and_then(|value| value.parse::<u64>().ok())
    .filter(|value| (5..=90).contains(value))
    .unwrap_or(25);
    let system = format!(
        "You are the SuperCampus admissions CRM copilot for tenant {}. Answer using the live portal context when it is supplied. Never tell the user to manually count data that exists in the context. The context is permission-scoped to the signed-in user. Never invent applicant data, never make final eligibility or admission decisions, and avoid discriminatory recommendations. {} Return ONLY JSON shaped as {{\"answer\":\"plain text answer\",\"action\":null}}. If the user explicitly asks to change a lead, action may instead be {{\"type\":\"add_lead_note|create_lead_task|move_lead\",\"leadQuery\":\"name, email, phone, or id from the user request\",\"payload\":{{}}}}. Use add_lead_note payload {{\"content\":\"...\"}}, create_lead_task payload {{\"title\":\"...\",\"dueAt\":\"RFC3339 timestamp\",\"priority\":\"low|medium|high|urgent\"}}, and move_lead payload {{\"toStage\":\"...\",\"toSubstate\":null,\"reason\":\"...\"}}. Never claim an action was completed; it must be confirmed by the user. Live portal context: {}",
        context.tenant,
        instruction,
        portal_context.as_ref().map(Value::to_string).unwrap_or_else(|| "unavailable for this user's permissions".into())
    );
    let payload = AiChatRequest {
        model: &model,
        messages: vec![
            AiChatRequestMessage {
                role: "system",
                content: &system,
            },
            AiChatRequestMessage {
                role: "user",
                content: input,
            },
        ],
        temperature: if intent == "extract" { 0.1 } else { 0.3 },
        max_tokens: 1400,
    };
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .map_err(|error| CrmHttpError(CrmError::ExternalService(error.to_string())))?
        .post(ai_chat_endpoint(&base_url))
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| CrmHttpError(CrmError::ExternalService(error.to_string())))?;
    if !response.status().is_success() {
        return Err(CrmHttpError(CrmError::ExternalService(format!(
            "AI returned HTTP {}",
            response.status()
        ))));
    }
    let response: AiChatResponse = response
        .json()
        .await
        .map_err(|error| CrmHttpError(CrmError::ExternalService(error.to_string())))?;
    let raw_content = response
        .choices
        .first()
        .map(|choice| choice.message.content.trim())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            CrmHttpError(CrmError::ExternalService(
                "AI returned an empty response".into(),
            ))
        })?
        .to_owned();
    let (content, action) = if let Some(planned) = parse_planner_response(&raw_content) {
        let action = match planned.action {
            Some(action) => resolve_action_proposal(&service, &context, action).await?,
            None => None,
        };
        let answer = if planned.answer.trim().is_empty() {
            action
                .as_ref()
                .map(|proposal| {
                    format!(
                        "I prepared this change for your review: {}. Confirm it below to update the portal.",
                        proposal.description
                    )
                })
                .unwrap_or_else(|| {
                    "I could not identify one exact lead for that action. Use the lead's full name, email, phone number, or complete ID and try again."
                        .into()
                })
        } else {
            planned.answer
        };
        (answer, action)
    } else {
        (raw_content, None)
    };
    Ok(ok(CrmTextAssistantResponse {
        content,
        intent,
        model,
        grounded: portal_context.is_some(),
        action,
    }))
}

pub async fn execute_assistant_action(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Json(request): Json<CrmAssistantActionRequest>,
) -> Result<Json<ApiResponse<Value>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let action = request.action;
    let result = match action.action_type.as_str() {
        "add_lead_note" => json!(service
            .add_lead_note(
                &context.tenant,
                &context.actor,
                action.lead_id,
                serde_json::from_value(action.payload)
                    .map_err(|error| CrmHttpError(CrmError::Validation(error.to_string())))?,
            )
            .await?),
        "create_lead_task" => service
            .add_lead_task(
                &context.tenant,
                &context.actor,
                action.lead_id,
                serde_json::from_value(action.payload)
                    .map_err(|error| CrmHttpError(CrmError::Validation(error.to_string())))?,
            )
            .await?,
        "move_lead" => json!(service
            .move_stage(
                &context.tenant,
                &context.actor,
                action.lead_id,
                serde_json::from_value(action.payload)
                    .map_err(|error| CrmHttpError(CrmError::Validation(error.to_string())))?,
            )
            .await?),
        _ => {
            return Err(CrmHttpError(CrmError::Validation(
                "unsupported assistant action".into(),
            )));
        }
    };
    Ok(ok(json!({
        "completed": true,
        "actionType": action.action_type,
        "leadId": action.lead_id,
        "leadName": action.lead_name,
        "result": result,
    })))
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
    Json(request): Json<DeleteLeadRequest>,
) -> Result<StatusCode, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    service
        .delete_lead(&context.tenant, &context.actor, id, request)
        .await?;
    let _ = state.realtime_wake.send(());
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

pub async fn transfer_candidates(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .transfer_candidates(&context.tenant, &context.actor)
        .await?))
}

pub async fn transfer_lead(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<TransferLeadRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let transferred = service
        .transfer_lead(&context.tenant, &context.actor, id, request)
        .await?;
    let _ = state.realtime_wake.send(());
    Ok(ok(transferred))
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
    let handoff = application_desk_handoff(&request.to_stage, request.to_substate.as_deref());
    let service = state.service(&context.tenant).await?;
    let moved = service
        .move_stage(&context.tenant, &context.actor, id, request)
        .await?;
    if moved.stage_key == "qualified"
        && let Err(error) = service
            .create_application_invitation(
                &context.tenant,
                &context.actor,
                id,
                CreateApplicationInvitationRequest { channel: None },
            )
            .await
    {
        tracing::warn!(
            tenant = %context.tenant,
            lead_id = %id,
            error = %error,
            "qualified lead moved, but application invitation could not be issued"
        );
    }
    if let Some(handoff) = handoff {
        state
            .reflect_application_in_desk(&context, &moved, handoff)
            .await?;
    }
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
    if decision.status == "approved"
        && let Some(handoff) =
            application_desk_handoff(&decision.to_stage, Some(&decision.to_substate))
    {
        let moved = service
            .get_lead(&context.tenant, &context.actor, decision.lead_id)
            .await?;
        state
            .reflect_application_in_desk(&context, &moved, handoff)
            .await?;
    }
    let _ = state.realtime_wake.send(());
    Ok(ok(decision))
}

#[cfg(test)]
mod application_desk_trigger_tests {
    use super::{
        ApplicationDeskHandoff, ai_chat_endpoint, application_desk_handoff, assistant_instruction,
        normalize_lead_query, parse_planner_response, pipeline_summary,
    };
    use serde_json::json;

    #[test]
    fn admission_desk_opens_only_after_offer_acceptance() {
        assert_eq!(application_desk_handoff("application", None), None);
        assert_eq!(application_desk_handoff("application", Some("to_do")), None);
        assert_eq!(
            application_desk_handoff("application", Some("application_in_progress")),
            None
        );
        assert_eq!(
            application_desk_handoff("application", Some("application_submitted")),
            None
        );
        assert_eq!(
            application_desk_handoff("application_status", Some("awaiting_decision")),
            None
        );
        assert_eq!(
            application_desk_handoff("offer_status", Some("to_do")),
            None
        );
        assert_eq!(
            application_desk_handoff("offer_status", Some("accepted")),
            Some(ApplicationDeskHandoff::OfferAccepted)
        );
        assert_eq!(
            application_desk_handoff("offer_status", Some("rejected")),
            None
        );
    }

    #[test]
    fn assistant_accepts_only_supported_tasks() {
        for intent in [
            "general",
            "summarize",
            "extract",
            "follow_up",
            "next_actions",
        ] {
            assert!(assistant_instruction(intent).is_some());
        }
        assert!(assistant_instruction("make_admission_decision").is_none());
    }

    #[test]
    fn assistant_accepts_host_or_v1_ai_base() {
        assert_eq!(
            ai_chat_endpoint("https://ai.example.test"),
            "https://ai.example.test/v1/chat/completions"
        );
        assert_eq!(
            ai_chat_endpoint("https://ai.example.test/v1/"),
            "https://ai.example.test/v1/chat/completions"
        );
    }

    #[test]
    fn assistant_parses_grounded_json_plan() {
        let response = parse_planner_response(
            r#"```json
            {"answer":"There are 4 leads.","action":null}
            ```"#,
        )
        .expect("valid planner response");
        assert_eq!(response.answer, "There are 4 leads.");
        assert!(response.action.is_none());
    }

    #[test]
    fn assistant_accepts_action_only_json_and_cleans_workspace_label() {
        let response = parse_planner_response(
            r#"{"action":{"type":"move_lead","leadQuery":"kamal Lead workspace · cae63ad1","payload":{"toStage":"nurture"}}}"#,
        )
        .expect("valid action-only response");
        assert!(response.answer.is_empty());
        assert_eq!(
            normalize_lead_query(&response.action.expect("action").lead_query),
            "kamal"
        );
    }

    #[test]
    fn assistant_counts_pipeline_substates() {
        let summary = pipeline_summary(&json!({
            "scope": "tenant",
            "total": 3,
            "stages": [{
                "key": "enquiry",
                "count": 3,
                "leads": [
                    {"substate_key": "contact_attempted"},
                    {"substate_key": "contact_attempted"},
                    {"substate_key": "new"}
                ]
            }]
        }));
        assert_eq!(summary["stages"][0]["substates"]["contact_attempted"], 2);
        assert_eq!(summary["total"], 3);
    }
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

pub async fn create_application_invitation(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<CreateApplicationInvitationRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let invitation = service
        .create_application_invitation(&context.tenant, &context.actor, id, request)
        .await?;
    let _ = state.realtime_wake.send(());
    Ok(ok(invitation))
}

pub async fn public_application_invitation(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(token): Path<Uuid>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::public_from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .public_application_invitation(&context.tenant, token)
        .await?))
}

pub async fn verify_application_otp(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(token): Path<Uuid>,
    Json(request): Json<VerifyApplicationOtpRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::public_from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    Ok(ok(service
        .verify_application_otp(&context.tenant, token, request)
        .await?))
}

pub async fn submit_invited_application(
    State(state): State<CrmApiState>,
    headers: HeaderMap,
    Path(token): Path<Uuid>,
    Json(request): Json<SubmitInvitedApplicationRequest>,
) -> Result<Json<ApiResponse<impl Serialize>>, CrmHttpError> {
    let context = RequestContext::public_from_headers(&headers)?;
    let service = state.service(&context.tenant).await?;
    let submission = service
        .submit_invited_application(&context.tenant, token, request)
        .await?;
    let _ = state.realtime_wake.send(());
    Ok(ok(submission))
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
