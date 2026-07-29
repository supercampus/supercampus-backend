use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportModuleRecord {
    pub tenant_id: String,
    pub id: String,
}
