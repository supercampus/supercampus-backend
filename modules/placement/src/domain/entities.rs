use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementModuleRecord {
    pub tenant_id: String,
    pub id: String,
}
