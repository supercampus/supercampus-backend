pub mod entities;
pub mod errors;
pub mod events;
pub mod value_objects;

pub use entities::Lead;
pub use errors::CrmError;
pub use value_objects::{PipelineKey, StageKey};
