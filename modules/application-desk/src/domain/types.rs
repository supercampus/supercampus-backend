//! Admission Desk — core domain types.
//!
//! The Admission Desk is an onboarding *orchestration* layer: it owns the
//! [`OnboardingCase`] and nothing else. Every other entity referenced here
//! (applicant, student, fee structure, user account) is owned by another module
//! and is referred to by id only.
//!
//! Serialization is camelCase because the finished web client consumes these
//! structures directly; the field names are part of the API contract.

use std::collections::BTreeMap;
use uuid::Uuid;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Forward pipeline position of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OnboardingStage {
    New,
    DataReview,
    IdentityVerification,
    DocumentVerification,
    AcademicMapping,
    SectionAllocation,
    FinanceVerification,
    Approval,
    StudentCreation,
    AccountProvisioning,
    AccessProvisioning,
    Activation,
    Completed,
}

impl OnboardingStage {
    pub const ALL: [Self; 13] = [
        Self::New,
        Self::DataReview,
        Self::IdentityVerification,
        Self::DocumentVerification,
        Self::AcademicMapping,
        Self::SectionAllocation,
        Self::FinanceVerification,
        Self::Approval,
        Self::StudentCreation,
        Self::AccountProvisioning,
        Self::AccessProvisioning,
        Self::Activation,
        Self::Completed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "NEW",
            Self::DataReview => "DATA_REVIEW",
            Self::IdentityVerification => "IDENTITY_VERIFICATION",
            Self::DocumentVerification => "DOCUMENT_VERIFICATION",
            Self::AcademicMapping => "ACADEMIC_MAPPING",
            Self::SectionAllocation => "SECTION_ALLOCATION",
            Self::FinanceVerification => "FINANCE_VERIFICATION",
            Self::Approval => "APPROVAL",
            Self::StudentCreation => "STUDENT_CREATION",
            Self::AccountProvisioning => "ACCOUNT_PROVISIONING",
            Self::AccessProvisioning => "ACCESS_PROVISIONING",
            Self::Activation => "ACTIVATION",
            Self::Completed => "COMPLETED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|stage| stage.as_str() == value)
    }
}

/// Lifecycle of the case, tracked independently of `stage` so that a held or
/// returned case still remembers where it must resume.
///
/// `Rejected` / `Cancelled` / `Withdrawn` / `Expired` are deliberately distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OnboardingStatus {
    Active,
    OnHold,
    Returned,
    Rejected,
    Cancelled,
    Withdrawn,
    Expired,
    Failed,
    Completed,
}

impl OnboardingStatus {
    /// Terminal statuses can never transition again.
    pub const TERMINAL: [Self; 5] = [
        Self::Rejected,
        Self::Cancelled,
        Self::Withdrawn,
        Self::Expired,
        Self::Completed,
    ];

    pub fn is_terminal(self) -> bool {
        Self::TERMINAL.contains(&self)
    }

    /// Statuses that mean a prior case must not block a legitimate re-admission.
    pub fn is_closed(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Cancelled | Self::Withdrawn | Self::Expired
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::OnHold => "ON_HOLD",
            Self::Returned => "RETURNED",
            Self::Rejected => "REJECTED",
            Self::Cancelled => "CANCELLED",
            Self::Withdrawn => "WITHDRAWN",
            Self::Expired => "EXPIRED",
            Self::Failed => "FAILED",
            Self::Completed => "COMPLETED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Active,
            Self::OnHold,
            Self::Returned,
            Self::Rejected,
            Self::Cancelled,
            Self::Withdrawn,
            Self::Expired,
            Self::Failed,
            Self::Completed,
        ]
        .into_iter()
        .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DocumentState {
    NotSubmitted,
    Submitted,
    UnderReview,
    Verified,
    Rejected,
    Expired,
    Waived,
}

impl DocumentState {
    /// A document counts as satisfied when verified outright or explicitly waived.
    pub fn is_satisfied(self) -> bool {
        matches!(self, Self::Verified | Self::Waived)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinanceState {
    NotRequired,
    Pending,
    Cleared,
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IdentityMatchKind {
    NoMatch,
    PossibleMatch,
    ConfirmedMatch,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRequirement {
    #[serde(rename = "type")]
    pub document_type: String,
    pub label: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRecord {
    #[serde(rename = "type")]
    pub document_type: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub state: DocumentState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file_id: Option<String>,
    /// Immutable Cloudinary metadata copied from the submitted Application form.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub secure_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uploaded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_form_field_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_submission_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_form_version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verified_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rejection_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcademicMapping {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub campus_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub department_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub program_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub academic_year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub semester: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub section_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub step: i32,
    pub role: String,
    pub state: ApprovalState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub comment: Option<String>,
}

/// Applicant contact details snapshotted at intake.
///
/// Frozen onto the case on purpose: account provisioning must produce the same
/// account on a retry, so it must not depend on a source record that may have
/// been edited in between.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicantSnapshot {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub full_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub guardian_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub guardian_email: Option<String>,
}

/// The onboarding case — the workflow record. The Student Master is only
/// created once the workflow reaches `STUDENT_CREATION`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingCase {
    pub id: String,
    pub tenant_id: String,
    pub applicant_id: String,
    pub application_id: String,
    pub admission_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub crm_lead_id: Option<Uuid>,

    pub stage: OnboardingStage,
    pub status: OnboardingStatus,
    /// Stage to return to when a hold or return is resolved.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resume_stage: Option<OnboardingStage>,

    pub workflow_id: String,
    pub workflow_version: i32,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub assigned_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub academic_year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub admission_category: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub identity_match: Option<IdentityMatchKind>,
    pub documents: Vec<DocumentRecord>,
    pub academic: AcademicMapping,
    pub finance: FinanceState,
    pub approvals: Vec<ApprovalRecord>,

    /// Contact facts copied in at intake; the admissions record stays authoritative.
    #[serde(default)]
    pub applicant: ApplicantSnapshot,

    /// Populated only by the corresponding provisioning stages.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub student_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub student_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub user_account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub access_provisioned: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hold_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rejection_reason: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub completed_at: Option<DateTime<Utc>>,

    /// Idempotency ledger: effect key -> result reference.
    #[serde(default)]
    pub applied_effects: BTreeMap<String, String>,

    /// Free-form institution fields addressed by workflow conditions.
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub case_id: String,
    pub actor: String,
    pub action: String,
    pub from_stage: OnboardingStage,
    pub to_stage: OnboardingStage,
    pub from_status: OnboardingStatus,
    pub to_status: OnboardingStatus,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}

/// Domain events other modules subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingEventName {
    OnboardingCreated,
    IdentityVerified,
    DocumentsVerified,
    AcademicMappingCompleted,
    SectionAllocated,
    FinanceVerified,
    StudentNumberGenerated,
    StudentCreated,
    UserCreated,
    AccessProvisioned,
    StudentActivated,
    OnboardingCompleted,
    OnboardingHeld,
    OnboardingReturned,
    OnboardingRejected,
    OnboardingFailed,
}

impl OnboardingEventName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnboardingCreated => "OnboardingCreated",
            Self::IdentityVerified => "IdentityVerified",
            Self::DocumentsVerified => "DocumentsVerified",
            Self::AcademicMappingCompleted => "AcademicMappingCompleted",
            Self::SectionAllocated => "SectionAllocated",
            Self::FinanceVerified => "FinanceVerified",
            Self::StudentNumberGenerated => "StudentNumberGenerated",
            Self::StudentCreated => "StudentCreated",
            Self::UserCreated => "UserCreated",
            Self::AccessProvisioned => "AccessProvisioned",
            Self::StudentActivated => "StudentActivated",
            Self::OnboardingCompleted => "OnboardingCompleted",
            Self::OnboardingHeld => "OnboardingHeld",
            Self::OnboardingReturned => "OnboardingReturned",
            Self::OnboardingRejected => "OnboardingRejected",
            Self::OnboardingFailed => "OnboardingFailed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingEvent {
    pub name: OnboardingEventName,
    pub case_id: String,
    pub tenant_id: String,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExceptionKind {
    DuplicateApplicant,
    MissingDocument,
    InvalidAcademicMapping,
    SectionUnavailable,
    FinanceHold,
    ApprovalRejected,
    ProvisioningFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingException {
    pub case_id: String,
    pub kind: ExceptionKind,
    pub message: String,
    pub retryable: bool,
}
