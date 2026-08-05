use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum CrmEvent {
    LeadCreated {
        lead_id: Uuid,
    },
    LeadDuplicateDetected {
        original_id: Uuid,
        duplicate_id: Uuid,
    },
    LeadAssigned {
        lead_id: Uuid,
        counselor_id: String,
        assignment_type: String,
    },
    LeadReassigned {
        lead_id: Uuid,
        old_counselor: Option<String>,
        new_counselor: String,
    },
    LeadStageChanged {
        lead_id: Uuid,
        from: String,
        to: String,
    },
    LeadArchived {
        lead_id: Uuid,
        reason: String,
    },
    LeadHold {
        lead_id: Uuid,
        reason: String,
    },
    CommunicationQueued {
        lead_id: Uuid,
        channel: String,
        template_key: Option<String>,
    },
    ErpHandoffRequested {
        lead_id: Uuid,
        payload: Value,
    },
}
