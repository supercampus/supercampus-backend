//! Admission Desk domain layer.
//!
//! Pure: no Axum, no SQLx, no messaging clients, no clock reads. The engine
//! receives `now` and its integration services from the caller, which is what
//! makes retries reproducible and the whole layer unit-testable.

pub mod default_workflow;
pub mod engine;
pub mod guards;
pub mod intake;
pub mod numbering;
pub mod types;
pub mod workflow;

pub use default_workflow::{default_workflow, international_workflow};
pub use engine::{
    ActionPayload, DocumentUpdate, EngineContext, OnboardingServices, ServiceError,
    TransitionResult, apply_action, effect_key,
};
pub use guards::{GuardResult, run_guards};
pub use intake::{
    AdmissionTrigger, CreateCaseOptions, IntakeDecision, IntakeTriggerMode, QueueKey,
    apply_application_document_mapping, average_onboarding_hours, create_case, evaluate_intake,
    queue_of, summarise_queues,
};
pub use numbering::{
    NumberToken, StudentNumberFormat, StudentNumberInput, department_code, format_student_number,
    sequence_scope,
};
pub use types::*;
pub use workflow::{
    ActionKind, ApprovalStep, Condition, ConditionOperator, EffectKind, GuardKey,
    WorkflowDefinition, WorkflowStage, WorkflowTransition,
};
