//! Workflow definition model + condition evaluator.
//!
//! The engine is data-driven: institutions describe stages, conditions and
//! transitions as configuration rather than the engine hard-coding an
//! `if`/`else` chain per institution.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{DocumentRequirement, OnboardingCase, OnboardingStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConditionOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Nin,
    Exists,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Dot path resolved against the case, e.g. `finance` or `academic.programId`.
    pub field: String,
    pub operator: ConditionOperator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// Named guards for checks that a field comparison cannot express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardKey {
    #[serde(rename = "mandatoryDocumentsSatisfied")]
    MandatoryDocumentsSatisfied,
    #[serde(rename = "identityResolved")]
    IdentityResolved,
    #[serde(rename = "academicMappingComplete")]
    AcademicMappingComplete,
    #[serde(rename = "sectionAllocated")]
    SectionAllocated,
    #[serde(rename = "financeSettled")]
    FinanceSettled,
    #[serde(rename = "approvalsComplete")]
    ApprovalsComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    Advance,
    Verify,
    Approve,
    Reject,
    Return,
    Hold,
    Resume,
    Cancel,
    Withdraw,
    Expire,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advance => "advance",
            Self::Verify => "verify",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Return => "return",
            Self::Hold => "hold",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
            Self::Withdraw => "withdraw",
            Self::Expire => "expire",
        }
    }

    /// The permission that gates this action. Verification, approval and
    /// activation deliberately sit on separate permissions so they can be held
    /// by different teams.
    pub fn required_permission(self) -> &'static str {
        match self {
            Self::Advance => "application-desk.edit",
            Self::Verify => "application-desk.verify",
            Self::Approve => "application-desk.approve",
            Self::Reject => "application-desk.reject",
            Self::Return => "application-desk.verify",
            Self::Hold => "application-desk.hold",
            Self::Resume => "application-desk.resume",
            Self::Cancel => "application-desk.reject",
            Self::Withdraw => "application-desk.reject",
            Self::Expire => "application-desk.manage_settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    GenerateNumber,
    CreateStudent,
    CreateUser,
    ProvisionAccess,
    Notify,
}

impl EffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerateNumber => "generate_number",
            Self::CreateStudent => "create_student",
            Self::CreateUser => "create_user",
            Self::ProvisionAccess => "provision_access",
            Self::Notify => "notify",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStage {
    pub id: OnboardingStage,
    pub label: String,
    pub sequence: i32,
    pub enabled: bool,
    /// A disabled non-mandatory stage is skipped during traversal.
    pub mandatory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_role: Option<String>,
    /// All must pass before the stage may be left via `advance`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<GuardKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Side effects executed on successful exit, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTransition {
    pub from: OnboardingStage,
    pub action: ActionKind,
    pub to: OnboardingStage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<GuardKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalStep {
    pub step: i32,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    pub id: String,
    pub version: i32,
    pub tenant_id: String,
    pub name: String,
    pub stages: Vec<WorkflowStage>,
    /// Explicit overrides; anything not listed falls back to sequence order.
    pub transitions: Vec<WorkflowTransition>,
    pub document_checklist: Vec<DocumentRequirement>,
    pub approval_chain: Vec<ApprovalStep>,
    /// Blank means the institution never auto-expires cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_days: Option<i32>,
}

impl WorkflowDefinition {
    pub fn stage(&self, stage: OnboardingStage) -> Option<&WorkflowStage> {
        self.stages.iter().find(|entry| entry.id == stage)
    }

    /// Next enabled stage in sequence order. Disabled stages are transparently
    /// skipped so an institution can switch a stage off without rewiring
    /// transitions.
    pub fn next_stage(&self, from: OnboardingStage) -> Option<OnboardingStage> {
        let current = self.stage(from)?;
        self.stages
            .iter()
            .filter(|stage| stage.enabled && stage.sequence > current.sequence)
            .min_by_key(|stage| stage.sequence)
            .map(|stage| stage.id)
    }
}

/// Resolve a dot path against the case's JSON projection without throwing on
/// missing links. Conditions address the case exactly as the client sees it,
/// so the camelCase serialization is the addressable surface.
pub fn resolve_field<'a>(source: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(source, |value, key| value.get(key))
        .filter(|value| !value.is_null())
}

fn as_number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn compare(actual: Option<&Value>, operator: ConditionOperator, expected: Option<&Value>) -> bool {
    match operator {
        ConditionOperator::Eq => actual == expected,
        ConditionOperator::Ne => actual != expected,
        ConditionOperator::Gt
        | ConditionOperator::Gte
        | ConditionOperator::Lt
        | ConditionOperator::Lte => {
            let (Some(left), Some(right)) =
                (actual.and_then(as_number), expected.and_then(as_number))
            else {
                return false;
            };
            match operator {
                ConditionOperator::Gt => left > right,
                ConditionOperator::Gte => left >= right,
                ConditionOperator::Lt => left < right,
                _ => left <= right,
            }
        }
        ConditionOperator::In => expected
            .and_then(Value::as_array)
            .is_some_and(|values| actual.is_some_and(|actual| values.contains(actual))),
        ConditionOperator::Nin => expected
            .and_then(Value::as_array)
            .is_some_and(|values| !actual.is_some_and(|actual| values.contains(actual))),
        ConditionOperator::Exists => {
            actual.is_some_and(|value| value != &Value::String(String::new()))
        }
        ConditionOperator::Empty => {
            actual.is_none() || actual == Some(&Value::String(String::new()))
        }
    }
}

pub fn evaluate_condition(projection: &Value, condition: &Condition) -> bool {
    compare(
        resolve_field(projection, &condition.field),
        condition.operator,
        condition.value.as_ref(),
    )
}

/// Conditions are conjunctive; an empty list is vacuously true.
pub fn evaluate_conditions(projection: &Value, conditions: &[Condition]) -> bool {
    conditions
        .iter()
        .all(|condition| evaluate_condition(projection, condition))
}

/// JSON view of a case, used as the addressing surface for conditions.
pub fn project(onboarding: &OnboardingCase) -> Value {
    serde_json::to_value(onboarding).unwrap_or(Value::Null)
}
