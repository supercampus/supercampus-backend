use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLeadRequest {
    pub source: String,
    #[serde(default)]
    pub source_detail: Value,
    pub student: StudentInput,
    #[serde(default)]
    pub academic: Value,
    #[serde(default)]
    pub interest: Value,
    #[serde(default)]
    pub communication: CommunicationPreferences,
    pub priority: Option<String>,
    pub follow_up_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub custom_fields: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkImportLeadsRequest {
    pub rows: Vec<BulkImportLeadRow>,
    pub duplicate_strategy: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkImportLeadRow {
    pub row_number: usize,
    #[serde(flatten)]
    pub lead: CreateLeadRequest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkImportLeadsResponse {
    pub total: usize,
    pub created: usize,
    pub skipped: usize,
    pub failed: usize,
    pub rows: Vec<BulkImportLeadResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkImportLeadResult {
    pub row_number: usize,
    pub status: String,
    pub lead_id: Option<Uuid>,
    pub duplicate_of: Option<Uuid>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentInput {
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub whatsapp: Option<String>,
    pub parent_name: Option<String>,
    pub parent_phone: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunicationPreferences {
    pub preferred_channel: Option<String>,
    #[serde(default)]
    pub consent_given: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLeadRequest {
    pub source: Option<String>,
    pub full_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub whatsapp: Option<String>,
    pub parent_name: Option<String>,
    pub parent_phone: Option<String>,
    pub academic: Option<Value>,
    pub interest: Option<Value>,
    pub priority: Option<String>,
    pub follow_up_at: Option<DateTime<Utc>>,
    pub fee_payment_confirmed: Option<bool>,
    pub documents_verified: Option<bool>,
    pub scholarship_status: Option<String>,
    pub custom_fields: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeadFilters {
    pub stage: Option<String>,
    pub substate: Option<String>,
    pub owner: Option<String>,
    pub source: Option<String>,
    pub global_status: Option<String>,
    pub priority: Option<String>,
    pub program_id: Option<String>,
    pub search: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub include_archived: Option<bool>,
    pub unassigned: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignLeadRequest {
    pub user_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TransferLeadRequest {
    pub user_id: String,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ClaimLeadRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveStageRequest {
    pub to_stage: String,
    pub to_substate: Option<String>,
    pub reason: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeStatusRequest {
    pub intake_year: i32,
    pub intake_month: Option<String>,
    pub program_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldRequest {
    pub reason: String,
    pub hold_until: Option<NaiveDate>,
    pub reminder_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
pub struct ReasonRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRequest {
    pub archive_reason: String,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnarchiveRequest {
    pub restore_to_stage: String,
    pub restore_to_substate: Option<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFormRequest {
    pub name: String,
    pub form_type: String,
    pub program_id: Option<String>,
    pub intake_year: Option<i32>,
    pub schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFormRequest {
    pub name: Option<String>,
    pub form_type: Option<String>,
    pub schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitFormRequest {
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub data: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendCommunicationRequest {
    pub lead_id: Uuid,
    pub template_key: Option<String>,
    pub subject: Option<String>,
    #[serde(default)]
    pub content: Value,
    pub outcome: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTemplateRequest {
    pub template_key: String,
    pub channel: String,
    pub name: String,
    pub content: String,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounselorCapacityRequest {
    pub user_id: String,
    pub display_name: String,
    pub active: Option<bool>,
    pub max_capacity: Option<i32>,
    #[serde(default)]
    pub source_categories: Value,
    #[serde(default)]
    pub program_ids: Value,
    #[serde(default)]
    pub territories: Value,
    pub average_response_minutes: Option<f64>,
    pub conversion_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowToggleRequest {
    pub from_stage: String,
    pub to_stage: String,
    #[serde(default)]
    pub allowed_roles: Value,
    pub requires_approval: Option<bool>,
    pub approval_role: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationToggleRequest {
    pub stage: String,
    pub trigger_name: String,
    pub action: String,
    pub template_key: Option<String>,
    #[serde(default)]
    pub conditions: Value,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCampaignRequest {
    pub name: String,
    pub source: String,
    pub budget: Option<f64>,
    pub spent: Option<f64>,
    pub attributed_revenue: Option<f64>,
    pub landing_pages: Option<i32>,
    pub utm_code: Option<String>,
    pub status: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub form_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveRequestDecision {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLeadNoteRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLeadTaskRequest {
    pub title: String,
    pub due_at: DateTime<Utc>,
    pub priority: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T> {
    pub data: T,
}

impl<T> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self { data }
    }
}
