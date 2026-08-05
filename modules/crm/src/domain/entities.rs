use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lead {
    pub id: Uuid,
    pub tenant_id: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub whatsapp: Option<String>,
    pub parent_name: Option<String>,
    pub parent_phone: Option<String>,
    pub source: String,
    pub source_detail: Value,
    pub academic: Value,
    pub interest: Value,
    pub pipeline_key: String,
    pub stage_key: String,
    pub substate_key: String,
    pub global_status: Option<String>,
    pub global_status_data: Value,
    pub assigned_to: Option<String>,
    pub assigned_by: Option<String>,
    pub assignment_type: Option<String>,
    pub priority: String,
    pub follow_up_at: Option<DateTime<Utc>>,
    pub preferred_channel: Option<String>,
    pub consent_given: bool,
    pub fee_payment_confirmed: bool,
    pub documents_verified: bool,
    pub scholarship_status: String,
    pub erp_status: String,
    pub erp_student_id: Option<String>,
    pub erp_enrollment_number: Option<String>,
    pub duplicate_of: Option<Uuid>,
    pub custom_fields: Value,
    pub created_by: String,
    pub stage_entered_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageHistoryEntry {
    pub id: Uuid,
    pub from_stage: Option<String>,
    pub from_substate: Option<String>,
    pub to_stage: String,
    pub to_substate: String,
    pub actor_id: String,
    pub actor_role: String,
    pub reason: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Communication {
    pub id: Uuid,
    pub lead_id: Uuid,
    pub channel: String,
    pub direction: String,
    pub template_key: Option<String>,
    pub subject: Option<String>,
    pub content: Value,
    pub outcome: Option<String>,
    pub status: String,
    pub actor_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormDefinition {
    pub id: Uuid,
    pub name: String,
    pub form_type: String,
    pub program_id: Option<String>,
    pub intake_year: Option<i32>,
    pub version: i32,
    pub status: String,
    pub schema: Value,
    pub created_by: String,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldRecord {
    pub reason: String,
    pub hold_until: Option<NaiveDate>,
    pub reminder_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Campaign {
    pub id: Uuid,
    pub name: String,
    pub source: String,
    pub budget: f64,
    pub spent: f64,
    pub attributed_revenue: f64,
    pub landing_pages: i32,
    pub utm_code: Option<String>,
    pub status: String,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
