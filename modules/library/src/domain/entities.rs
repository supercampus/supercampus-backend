use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryModuleRecord {
    pub tenant_id: String,
    pub id: String,
}
