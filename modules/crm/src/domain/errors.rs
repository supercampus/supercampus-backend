use thiserror::Error;

#[derive(Debug, Error)]
pub enum CrmError {
    #[error("CRM record was not found")]
    NotFound,
    #[error("authentication is required")]
    Unauthorized,
    #[error("permission denied: {0}")]
    Forbidden(String),
    #[error("CRM validation failed: {0}")]
    Validation(String),
    #[error("CRM record conflicts with existing data: {0}")]
    Conflict(String),
    #[error("CRM storage operation failed: {0}")]
    Storage(String),
    #[error("CRM database is not configured")]
    Unavailable,
    #[error("CRM external service failed: {0}")]
    ExternalService(String),
}
