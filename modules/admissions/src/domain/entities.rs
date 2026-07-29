use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionsModuleRecord {
    pub tenant_id: String,
    pub id: String,
}
