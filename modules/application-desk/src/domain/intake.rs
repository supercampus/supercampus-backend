//! Case intake and the dashboard queue projection.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    types::{
        AcademicMapping, ApplicantSnapshot, ApprovalRecord, ApprovalState, DocumentRecord,
        DocumentState, FinanceState, OnboardingCase, OnboardingStage, OnboardingStatus,
    },
    workflow::WorkflowDefinition,
};

/// The admission facts the desk copies in; source records stay authoritative.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionTrigger {
    /// Always taken from the authenticated request, never from the body.
    #[serde(default)]
    pub tenant_id: String,
    pub applicant_id: String,
    pub application_id: String,
    pub admission_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crm_lead_id: Option<Uuid>,
    pub admission_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub academic_year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campus_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(default)]
    pub fee_paid: bool,
    /// Contact facts frozen onto the case so provisioning stays reproducible.
    #[serde(default)]
    pub applicant: ApplicantSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeTriggerMode {
    #[default]
    OnConfirmed,
    OnFeePaid,
    Manual,
}

impl IntakeTriggerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnConfirmed => "on_confirmed",
            Self::OnFeePaid => "on_fee_paid",
            Self::Manual => "manual",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "on_confirmed" => Some(Self::OnConfirmed),
            "on_fee_paid" => Some(Self::OnFeePaid),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeDecision {
    pub create: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
}

/// Decide whether a trigger should open a case.
///
/// Duplicate protection checks applicant, application and admission ids against
/// live cases. A closed case must not block a legitimate re-admission.
pub fn evaluate_intake(
    trigger: &AdmissionTrigger,
    existing: &[OnboardingCase],
    mode: IntakeTriggerMode,
) -> IntakeDecision {
    if mode == IntakeTriggerMode::OnConfirmed && trigger.admission_status != "CONFIRMED" {
        return IntakeDecision {
            create: false,
            reason: format!("Admission is {}, not CONFIRMED", trigger.admission_status),
            duplicate_of: None,
        };
    }
    if mode == IntakeTriggerMode::OnFeePaid && !trigger.fee_paid {
        return IntakeDecision {
            create: false,
            reason: "Admission fee has not been paid".into(),
            duplicate_of: None,
        };
    }

    let duplicate = existing.iter().find(|entry| {
        entry.applicant_id == trigger.applicant_id
            || entry.application_id == trigger.application_id
            || entry.admission_id == trigger.admission_id
            || (trigger.crm_lead_id.is_some() && entry.crm_lead_id == trigger.crm_lead_id)
    });

    if let Some(duplicate) = duplicate
        && !duplicate.status.is_closed()
    {
        return IntakeDecision {
            create: false,
            reason: format!(
                "Applicant already has onboarding case {} ({})",
                duplicate.id,
                duplicate.status.as_str()
            ),
            duplicate_of: Some(duplicate.id.clone()),
        };
    }

    IntakeDecision {
        create: true,
        reason: "Eligible for onboarding".into(),
        duplicate_of: None,
    }
}

pub struct CreateCaseOptions {
    pub id: String,
    pub now: DateTime<Utc>,
    pub assigned_to: Option<String>,
}

/// Build a case seeded from admission data.
pub fn create_case(
    trigger: &AdmissionTrigger,
    definition: &WorkflowDefinition,
    options: CreateCaseOptions,
) -> OnboardingCase {
    let documents: Vec<DocumentRecord> = definition
        .document_checklist
        .iter()
        .map(|requirement| DocumentRecord {
            document_type: requirement.document_type.clone(),
            state: DocumentState::NotSubmitted,
            file_id: None,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            expires_at: None,
        })
        .collect();

    OnboardingCase {
        id: options.id,
        tenant_id: trigger.tenant_id.clone(),
        applicant_id: trigger.applicant_id.clone(),
        application_id: trigger.application_id.clone(),
        admission_id: trigger.admission_id.clone(),
        crm_lead_id: trigger.crm_lead_id,
        stage: OnboardingStage::DataReview,
        status: OnboardingStatus::Active,
        resume_stage: None,
        workflow_id: definition.id.clone(),
        workflow_version: definition.version,
        assigned_to: options.assigned_to,
        academic_year: trigger.academic_year.clone(),
        admission_category: trigger.admission_category.clone(),
        identity_match: None,
        documents,
        academic: AcademicMapping {
            campus_id: trigger.campus_id.clone(),
            department_id: trigger.department_id.clone(),
            program_id: trigger.program_id.clone(),
            academic_year: trigger.academic_year.clone(),
            batch_id: trigger.batch_id.clone(),
            semester: None,
            section_id: None,
        },
        finance: if trigger.fee_paid {
            FinanceState::Cleared
        } else {
            FinanceState::Pending
        },
        approvals: definition
            .approval_chain
            .iter()
            .map(|step| ApprovalRecord {
                step: step.step,
                role: step.role.clone(),
                state: ApprovalState::Pending,
                acted_by: None,
                acted_at: None,
                comment: None,
            })
            .collect(),
        applicant: trigger.applicant.clone(),
        student_number: None,
        student_id: None,
        user_account_id: None,
        access_provisioned: None,
        hold_reason: None,
        rejection_reason: None,
        created_at: options.now,
        updated_at: options.now,
        completed_at: None,
        applied_effects: BTreeMap::new(),
        attributes: serde_json::Map::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueueKey {
    New,
    PendingVerification,
    DocumentsPending,
    AcademicPending,
    FinancePending,
    ApprovalPending,
    ReadyForActivation,
    Activated,
    OnHold,
    Rejected,
    Failed,
}

impl QueueKey {
    pub const ALL: [Self; 11] = [
        Self::New,
        Self::PendingVerification,
        Self::DocumentsPending,
        Self::AcademicPending,
        Self::FinancePending,
        Self::ApprovalPending,
        Self::ReadyForActivation,
        Self::Activated,
        Self::OnHold,
        Self::Rejected,
        Self::Failed,
    ];

    /// camelCase to match the queue keys the dashboard already renders.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::PendingVerification => "pendingVerification",
            Self::DocumentsPending => "documentsPending",
            Self::AcademicPending => "academicPending",
            Self::FinancePending => "financePending",
            Self::ApprovalPending => "approvalPending",
            Self::ReadyForActivation => "readyForActivation",
            Self::Activated => "activated",
            Self::OnHold => "onHold",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

/// Which dashboard bucket a case belongs to.
pub fn queue_of(onboarding: &OnboardingCase) -> QueueKey {
    match onboarding.status {
        OnboardingStatus::OnHold | OnboardingStatus::Returned => return QueueKey::OnHold,
        OnboardingStatus::Rejected
        | OnboardingStatus::Cancelled
        | OnboardingStatus::Withdrawn
        | OnboardingStatus::Expired => return QueueKey::Rejected,
        OnboardingStatus::Failed => return QueueKey::Failed,
        OnboardingStatus::Completed => return QueueKey::Activated,
        OnboardingStatus::Active => {}
    }

    match onboarding.stage {
        OnboardingStage::New | OnboardingStage::DataReview => QueueKey::New,
        OnboardingStage::IdentityVerification => QueueKey::PendingVerification,
        OnboardingStage::DocumentVerification => QueueKey::DocumentsPending,
        OnboardingStage::AcademicMapping | OnboardingStage::SectionAllocation => {
            QueueKey::AcademicPending
        }
        OnboardingStage::FinanceVerification => QueueKey::FinancePending,
        OnboardingStage::Approval => QueueKey::ApprovalPending,
        _ => QueueKey::ReadyForActivation,
    }
}

pub fn summarise_queues(cases: &[OnboardingCase]) -> BTreeMap<&'static str, usize> {
    let mut counts: BTreeMap<&'static str, usize> = QueueKey::ALL
        .into_iter()
        .map(|key| (key.as_str(), 0))
        .collect();
    for entry in cases {
        *counts.entry(queue_of(entry).as_str()).or_insert(0) += 1;
    }
    counts
}

/// Average completed-onboarding duration in hours.
pub fn average_onboarding_hours(cases: &[OnboardingCase]) -> Option<f64> {
    let completed: Vec<&OnboardingCase> = cases
        .iter()
        .filter(|entry| entry.completed_at.is_some())
        .collect();
    if completed.is_empty() {
        return None;
    }
    let total: f64 = completed
        .iter()
        .map(|entry| {
            let finished = entry.completed_at.unwrap_or(entry.created_at);
            let millis = (finished - entry.created_at).num_milliseconds().max(0);
            millis as f64
        })
        .sum();
    let average = total / completed.len() as f64 / 3_600_000.0;
    Some((average * 10.0).round() / 10.0)
}
