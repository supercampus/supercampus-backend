use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use supercampus_kernel::TenantId;
use uuid::Uuid;

use super::{PipelineKey, StageKey};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lead {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub full_name: String,
    pub email: Option<String>,
    pub pipeline: PipelineKey,
    pub stage: StageKey,
    pub custom_fields: Value,
    pub created_at: DateTime<Utc>,
}
