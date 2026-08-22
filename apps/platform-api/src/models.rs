use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T> {
    pub data: T,
}

impl<T> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleDescriptor {
    pub key: String,
    pub name: String,
    pub version: String,
    pub base_path: String,
    pub status: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDescriptor {
    pub key: String,
    pub name: String,
    pub base_path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationItem {
    pub id: String,
    pub label: String,
    pub route: String,
    pub required_permission: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapDocument {
    pub tenant_id: String,
    pub user_id: String,
    pub tenant_brand: Value,
    pub roles: Vec<String>,
    pub portal_families: Vec<String>,
    pub permissions: Vec<String>,
    pub permission_scopes: HashMap<String, String>,
    pub workflows: Vec<WorkflowDefinition>,
    pub services: Vec<ServiceDescriptor>,
    pub modules: Vec<ModuleDescriptor>,
    pub navigation: Vec<NavigationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicRecord {
    pub id: Uuid,
    pub tenant_id: String,
    pub module_key: String,
    pub record_type: String,
    pub data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRecordRequest {
    pub record_type: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRecordRequest {
    pub data: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentImportRow {
    pub name: String,
    pub roll_no: String,
    pub department: String,
    pub mobile_number: String,
    pub email: String,
}

/// Sets or clears a student's photograph. `null` removes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentPhotoRequest {
    #[serde(default)]
    pub photo_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkStudentImportRequest {
    pub rows: Vec<StudentImportRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationDocument {
    pub tenant_id: String,
    pub namespace: String,
    pub version: u64,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct PutConfigurationRequest {
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    pub tenant_id: String,
    pub module: String,
    pub feature: String,
    pub version: u64,
    pub initial_state: String,
    pub terminal_states: Vec<String>,
    pub states: Vec<WorkflowState>,
    pub transitions: Vec<WorkflowTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowState {
    pub id: String,
    pub label: String,
    pub status: WorkflowStateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStateStatus {
    Draft,
    Pending,
    Approved,
    Rejected,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTransition {
    pub from: String,
    pub to: String,
    pub action: String,
    pub required_permission: String,
    pub required_role: Option<String>,
    pub label: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateWorkflowTransitionRequest {
    pub current_state: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveWidget {
    pub id: String,
    pub required_permission: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAuthorizationRoleRequest {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default = "default_portal_family")]
    pub portal_family: String,
    #[serde(default = "default_role_surfaces")]
    pub surfaces: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAuthorizationRoleRequest {
    pub name: Option<String>,
    pub team: Option<String>,
    pub scope: Option<String>,
    pub active: Option<bool>,
    pub portal_family: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionGrantRequest {
    pub key: String,
    #[serde(default = "default_permission_scope")]
    pub scope: String,
    #[serde(default)]
    pub constraints: Value,
}

#[derive(Debug, Deserialize)]
pub struct SetRolePermissionsRequest {
    #[serde(default = "default_website_surface")]
    pub surface: String,
    pub permissions: Vec<PermissionGrantRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTenantUserRequest {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub role_ids: Vec<Uuid>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub temporary_password: Option<String>,
}

impl CreateTenantUserRequest {
    pub fn credential_password(&self) -> Option<&str> {
        self.temporary_password
            .as_deref()
            .or(self.password.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignUserRolesRequest {
    pub role_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetUserAccessRequest {
    pub surface: String,
    pub grants: Vec<DirectPermissionGrantRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectPermissionGrantRequest {
    pub key: String,
    #[serde(default = "default_permission_scope")]
    pub scope: String,
    #[serde(default = "default_permission_mode")]
    pub mode: String,
    #[serde(default)]
    pub constraints: Value,
}

fn default_permission_scope() -> String {
    "all".into()
}

fn default_portal_family() -> String {
    "staff".into()
}

fn default_role_surfaces() -> Vec<String> {
    vec!["website".into(), "app".into()]
}

fn default_website_surface() -> String {
    "website".into()
}

fn default_permission_mode() -> String {
    "allow".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub session_mode: SessionMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Cookie,
    Token,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RefreshRequest {
    pub refresh_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogoutRequest {
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantSummary {
    pub id: String,
    pub code: String,
    pub name: String,
    pub city: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStudent {
    pub id: String,
    pub tenant_id: String,
    pub email: String,
    pub name: String,
    pub initials: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub portal_families: Vec<String>,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub access: Vec<String>,
    pub roll: String,
    pub college: String,
    pub dept: String,
    pub year: String,
    pub full_college: String,
    pub tenant: TenantSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginData {
    pub student: AuthStudent,
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_at: DateTime<Utc>,
    pub session_id: Uuid,
    pub roles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    pub student: AuthStudent,
    pub session_id: Uuid,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAppState {
    pub state: Value,
    pub version: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct SaveAppStateRequest {
    pub state: Value,
    pub action: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthDocument {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
}
