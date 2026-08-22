//! The default MVP workflow.
//!
//! Note on ordering: the narrative spec lists student-number generation before
//! approval, while the state model and the worked example both generate it
//! *after* approval succeeds. We follow the latter — minting an institutional
//! identifier for a case that may still be rejected would burn sequence numbers
//! and leak identifiers for non-students. The number is an effect of
//! `STUDENT_CREATION`.

use super::{
    types::{DocumentRequirement, OnboardingStage},
    workflow::{
        ActionKind, ApprovalStep, EffectKind, GuardKey, WorkflowDefinition, WorkflowStage,
        WorkflowTransition,
    },
};

pub const DEFAULT_WORKFLOW_ID: &str = "application-desk-default";
pub const DEFAULT_WORKFLOW_VERSION: i32 = 1;

fn requirement(document_type: &str, label: &str, required: bool) -> DocumentRequirement {
    DocumentRequirement {
        document_type: document_type.into(),
        label: label.into(),
        required,
    }
}

/// Safe baseline used when a tenant has not supplied a published form schema.
/// Submitted application-form requirements replace this checklist at intake.
pub fn default_document_checklist() -> Vec<DocumentRequirement> {
    vec![
        requirement("certificate-10", "10th Certificate", true),
        requirement("certificate-12", "12th Certificate", true),
        requirement("transfer-certificate", "Transfer Certificate", true),
        requirement("identity-proof", "Identity Proof", true),
        requirement("address-proof", "Address Proof", false),
        requirement("photo", "Passport Photo", true),
        requirement("admission-proof", "Admission Proof", true),
        requirement("category-certificate", "Category Certificate", false),
    ]
}

fn stage(
    id: OnboardingStage,
    label: &str,
    sequence: i32,
    assigned_role: Option<&str>,
    guards: Vec<GuardKey>,
    effects: Vec<EffectKind>,
) -> WorkflowStage {
    WorkflowStage {
        id,
        label: label.into(),
        sequence,
        enabled: true,
        mandatory: true,
        assigned_role: assigned_role.map(str::to_owned),
        guards,
        conditions: Vec::new(),
        effects,
    }
}

pub fn default_stages() -> Vec<WorkflowStage> {
    vec![
        stage(
            OnboardingStage::New,
            "Case Created",
            0,
            None,
            vec![],
            vec![],
        ),
        stage(
            OnboardingStage::DataReview,
            "Applicant Data Review",
            10,
            Some("application-desk-officer"),
            vec![],
            vec![],
        ),
        stage(
            OnboardingStage::IdentityVerification,
            "Identity Verification",
            20,
            Some("application-desk-officer"),
            vec![GuardKey::IdentityResolved],
            vec![],
        ),
        stage(
            OnboardingStage::DocumentVerification,
            "Document Verification",
            30,
            Some("application-desk-officer"),
            vec![GuardKey::MandatoryDocumentsSatisfied],
            vec![],
        ),
        stage(
            OnboardingStage::AcademicMapping,
            "Academic Mapping",
            40,
            Some("academic-administrator"),
            vec![GuardKey::AcademicMappingComplete],
            vec![],
        ),
        stage(
            OnboardingStage::SectionAllocation,
            "Section Allocation",
            50,
            Some("academic-administrator"),
            vec![GuardKey::SectionAllocated],
            vec![],
        ),
        stage(
            OnboardingStage::FinanceVerification,
            "Finance Verification",
            60,
            Some("finance-officer"),
            vec![GuardKey::FinanceSettled],
            vec![],
        ),
        stage(
            OnboardingStage::Approval,
            "Approval",
            70,
            Some("registrar"),
            vec![GuardKey::ApprovalsComplete],
            vec![],
        ),
        stage(
            OnboardingStage::StudentCreation,
            "Student Master Creation",
            80,
            None,
            vec![],
            // Number first, then the student record that carries it.
            vec![EffectKind::GenerateNumber, EffectKind::CreateStudent],
        ),
        stage(
            OnboardingStage::AccountProvisioning,
            "User Account Creation",
            90,
            None,
            vec![],
            vec![EffectKind::CreateUser],
        ),
        stage(
            OnboardingStage::AccessProvisioning,
            "Module Access Provisioning",
            100,
            None,
            vec![],
            vec![EffectKind::ProvisionAccess],
        ),
        stage(
            OnboardingStage::Activation,
            "Activation & Welcome",
            110,
            None,
            vec![],
            vec![EffectKind::Notify],
        ),
        stage(
            OnboardingStage::Completed,
            "Completed",
            120,
            None,
            vec![],
            vec![],
        ),
    ]
}

pub fn default_workflow(tenant_id: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        id: DEFAULT_WORKFLOW_ID.into(),
        version: DEFAULT_WORKFLOW_VERSION,
        tenant_id: tenant_id.into(),
        name: "Standard Admission Onboarding".into(),
        stages: default_stages(),
        // Sequence order covers the happy path; only exceptions need declaring.
        transitions: vec![
            WorkflowTransition {
                from: OnboardingStage::Approval,
                action: ActionKind::Reject,
                to: OnboardingStage::Approval,
                when: Vec::new(),
                guards: Vec::new(),
            },
            WorkflowTransition {
                from: OnboardingStage::DocumentVerification,
                action: ActionKind::Return,
                to: OnboardingStage::DataReview,
                when: Vec::new(),
                guards: Vec::new(),
            },
        ],
        document_checklist: default_document_checklist(),
        approval_chain: vec![
            ApprovalStep {
                step: 1,
                role: "application-desk-officer".into(),
            },
            ApprovalStep {
                step: 2,
                role: "registrar".into(),
            },
        ],
        expiry_days: Some(45),
    }
}

/// International applicants need passport/visa checks the standard flow skips.
/// The same engine drives both without a code change.
pub fn international_workflow(tenant_id: &str) -> WorkflowDefinition {
    let mut definition = default_workflow(tenant_id);
    definition.id = "application-desk-international".into();
    definition.name = "International Admission Onboarding".into();
    definition
        .document_checklist
        .push(requirement("passport", "Passport", true));
    definition
        .document_checklist
        .push(requirement("visa", "Student Visa", true));
    definition.approval_chain.push(ApprovalStep {
        step: 3,
        role: "international-office".into(),
    });
    definition
}
