use async_trait::async_trait;
use supercampus_kernel::TenantId;
use uuid::Uuid;

use crate::domain::{CrmError, Lead};

#[async_trait]
pub trait LeadRepository: Send + Sync {
    async fn find(&self, tenant_id: TenantId, lead_id: Uuid) -> Result<Option<Lead>, CrmError>;
    async fn save(&self, lead: &Lead) -> Result<(), CrmError>;
}
