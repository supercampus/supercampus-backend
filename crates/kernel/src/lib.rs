#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub Uuid);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub campus_id: Option<Uuid>,
    pub department_id: Option<Uuid>,
    pub permissions: Vec<String>,
}
