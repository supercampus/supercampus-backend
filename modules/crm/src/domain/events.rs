use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CrmEvent {
    LeadCreated {
        lead_id: Uuid,
    },
    LeadStageChanged {
        lead_id: Uuid,
        from: String,
        to: String,
    },
}
