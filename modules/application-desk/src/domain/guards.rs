//! Named guards. Each answers one question about a case and returns a reason
//! when it blocks, so the desk can show the operator *why* a stage will not
//! advance instead of a bare "not allowed".

use super::{
    types::{ApprovalState, FinanceState, IdentityMatchKind, OnboardingCase},
    workflow::{GuardKey, WorkflowDefinition},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardResult {
    pub ok: bool,
    pub reason: Option<String>,
}

impl GuardResult {
    fn pass() -> Self {
        Self {
            ok: true,
            reason: None,
        }
    }

    fn block(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
        }
    }
}

/// Every mandatory document from the checklist is VERIFIED or WAIVED.
fn mandatory_documents_satisfied(
    onboarding: &OnboardingCase,
    _: &WorkflowDefinition,
) -> GuardResult {
    let outstanding: Vec<&str> = onboarding
        .documents
        .iter()
        .filter(|record| record.required && !record.state.is_satisfied())
        .map(|record| record.label.as_deref().unwrap_or(&record.document_type))
        .collect();

    if outstanding.is_empty() {
        return GuardResult::pass();
    }
    GuardResult::block(format!(
        "{} mandatory document(s) outstanding: {}",
        outstanding.len(),
        outstanding.join(", ")
    ))
}

/// A duplicate must never proceed; a possible match needs a human first.
fn identity_resolved(onboarding: &OnboardingCase, _: &WorkflowDefinition) -> GuardResult {
    match onboarding.identity_match {
        Some(IdentityMatchKind::NoMatch | IdentityMatchKind::ConfirmedMatch) => GuardResult::pass(),
        Some(IdentityMatchKind::PossibleMatch) => {
            GuardResult::block("Possible identity match awaiting manual review")
        }
        Some(IdentityMatchKind::Duplicate) => {
            GuardResult::block("Applicant is a confirmed duplicate — onboarding blocked")
        }
        None => GuardResult::block("Identity verification has not been run"),
    }
}

/// Academic structure must be fully resolved before allocation.
fn academic_mapping_complete(onboarding: &OnboardingCase, _: &WorkflowDefinition) -> GuardResult {
    let academic = &onboarding.academic;
    let missing: Vec<&str> = [
        ("programId", academic.program_id.as_ref()),
        ("departmentId", academic.department_id.as_ref()),
        ("academicYear", academic.academic_year.as_ref()),
        ("batchId", academic.batch_id.as_ref()),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_none_or(|value| value.is_empty()))
    .map(|(key, _)| key)
    .collect();

    if missing.is_empty() {
        return GuardResult::pass();
    }
    GuardResult::block(format!(
        "Academic mapping incomplete: {}",
        missing.join(", ")
    ))
}

fn section_allocated(onboarding: &OnboardingCase, _: &WorkflowDefinition) -> GuardResult {
    match onboarding.academic.section_id.as_deref() {
        Some(section) if !section.is_empty() => GuardResult::pass(),
        _ => GuardResult::block("Section has not been allocated"),
    }
}

/// Institutions may allow creation before clearance; only HOLD hard-blocks.
fn finance_settled(onboarding: &OnboardingCase, _: &WorkflowDefinition) -> GuardResult {
    match onboarding.finance {
        FinanceState::Cleared | FinanceState::NotRequired => GuardResult::pass(),
        FinanceState::Pending => GuardResult::block("Finance verification is still pending"),
        FinanceState::Hold => GuardResult::block("Case is on a finance hold"),
    }
}

/// Every configured approval step has been APPROVED.
fn approvals_complete(onboarding: &OnboardingCase, definition: &WorkflowDefinition) -> GuardResult {
    if definition.approval_chain.is_empty() {
        return GuardResult::pass();
    }
    let outstanding: Vec<&str> = definition
        .approval_chain
        .iter()
        .filter(|step| {
            !onboarding
                .approvals
                .iter()
                .any(|record| record.step == step.step && record.state == ApprovalState::Approved)
        })
        .map(|step| step.role.as_str())
        .collect();

    if outstanding.is_empty() {
        return GuardResult::pass();
    }
    GuardResult::block(format!(
        "Awaiting approval from: {}",
        outstanding.join(", ")
    ))
}

fn evaluate(
    key: GuardKey,
    onboarding: &OnboardingCase,
    definition: &WorkflowDefinition,
) -> GuardResult {
    match key {
        GuardKey::MandatoryDocumentsSatisfied => {
            mandatory_documents_satisfied(onboarding, definition)
        }
        GuardKey::IdentityResolved => identity_resolved(onboarding, definition),
        GuardKey::AcademicMappingComplete => academic_mapping_complete(onboarding, definition),
        GuardKey::SectionAllocated => section_allocated(onboarding, definition),
        GuardKey::FinanceSettled => finance_settled(onboarding, definition),
        GuardKey::ApprovalsComplete => approvals_complete(onboarding, definition),
    }
}

/// Run guards and collect *every* failure rather than short-circuiting — a
/// parallel stage gate should tell the operator all of what is outstanding at
/// once.
pub fn run_guards(
    onboarding: &OnboardingCase,
    definition: &WorkflowDefinition,
    keys: &[GuardKey],
) -> GuardResult {
    let reasons: Vec<String> = keys
        .iter()
        .map(|key| evaluate(*key, onboarding, definition))
        .filter(|result| !result.ok)
        .map(|result| result.reason.unwrap_or_else(|| "blocked".into()))
        .collect();

    if reasons.is_empty() {
        GuardResult::pass()
    } else {
        GuardResult::block(reasons.join("; "))
    }
}
