use thiserror::Error;

#[derive(Debug, Error)]
pub enum CrmError {
    #[error("CRM record was not found")]
    NotFound,
    #[error("CRM validation failed: {0}")]
    Validation(String),
    #[error("CRM storage operation failed")]
    Storage,
}
