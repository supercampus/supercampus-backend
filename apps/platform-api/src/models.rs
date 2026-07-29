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

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
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
    pub roll: String,
    pub college: String,
    pub dept: String,
    pub year: String,
    pub full_college: String,
    pub tenant: TenantSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginData {
    pub student: AuthStudent,
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
