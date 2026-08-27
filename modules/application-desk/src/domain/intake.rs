//! Case intake and the dashboard queue projection.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// Source-system facts copied at intake for operator context. The source
    /// record remains authoritative; this is an immutable onboarding snapshot.
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
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
    let offer_accepted = trigger
        .attributes
        .get("handoffReason")
        .and_then(serde_json::Value::as_str)
        == Some("offer_accepted");
    if mode == IntakeTriggerMode::OnFeePaid && !trigger.fee_paid && !offer_accepted {
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::domain::{OnboardingStage, OnboardingStatus};

    fn accepted_trigger() -> AdmissionTrigger {
        AdmissionTrigger {
            applicant_id: "applicant-1".into(),
            application_id: "application-1".into(),
            admission_id: "admission-1".into(),
            admission_status: "CONFIRMED".into(),
            attributes: serde_json::json!({
                "handoffReason": "offer_accepted"
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
            ..AdmissionTrigger::default()
        }
    }

    fn existing_case() -> OnboardingCase {
        OnboardingCase {
            id: "case-1".into(),
            tenant_id: "tenant-1".into(),
            applicant_id: "applicant-1".into(),
            application_id: "application-1".into(),
            admission_id: "admission-1".into(),
            crm_lead_id: None,
            stage: OnboardingStage::DataReview,
            status: OnboardingStatus::Active,
            resume_stage: None,
            workflow_id: "workflow".into(),
            workflow_version: 1,
            assigned_to: None,
            academic_year: None,
            admission_category: None,
            identity_match: None,
            documents: vec![],
            academic: AcademicMapping::default(),
            finance: FinanceState::Pending,
            approvals: vec![],
            applicant: ApplicantSnapshot::default(),
            student_number: None,
            student_id: None,
            user_account_id: None,
            access_provisioned: None,
            hold_reason: None,
            rejection_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            completed_at: None,
            applied_effects: BTreeMap::new(),
            attributes: Default::default(),
        }
    }

    #[test]
    fn accepted_offer_is_a_valid_admission_desk_handoff() {
        let decision = evaluate_intake(&accepted_trigger(), &[], IntakeTriggerMode::OnConfirmed);
        assert!(decision.create);
    }

    #[test]
    fn duplicate_offer_acceptance_reuses_the_live_case() {
        let decision = evaluate_intake(
            &accepted_trigger(),
            &[existing_case()],
            IntakeTriggerMode::OnConfirmed,
        );
        assert!(!decision.create);
        assert_eq!(decision.duplicate_of.as_deref(), Some("case-1"));
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
    let mut documents: Vec<DocumentRecord> = definition
        .document_checklist
        .iter()
        .map(|requirement| DocumentRecord {
            document_type: requirement.document_type.clone(),
            label: Some(requirement.label.clone()),
            required: requirement.required,
            state: DocumentState::NotSubmitted,
            file_id: None,
            file_name: None,
            content_type: None,
            secure_url: None,
            bytes: None,
            uploaded_at: None,
            source_form_field_key: None,
            source_submission_id: None,
            source_form_version: None,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            expires_at: None,
        })
        .collect();

    if trigger.attributes.contains_key("applicationForm") {
        apply_application_document_mapping(&mut documents, &trigger.attributes);
    }

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
        attributes: trigger.attributes.clone(),
    }
}

/// Populate checklist records from the immutable submitted Application form snapshot.
/// Payloads from every other form type are deliberately ignored.
pub fn apply_application_document_mapping(
    documents: &mut Vec<DocumentRecord>,
    attributes: &serde_json::Map<String, Value>,
) {
    let Some(application) = attributes.get("applicationForm").and_then(Value::as_object) else {
        // Cases without an Application-form snapshot still use the workflow
        // checklist. Do not erase facts already recorded against that checklist.
        return;
    };
    // Once an Application-form snapshot exists it is authoritative. Records
    // are rebuilt only from a valid submitted Application form below; a
    // similarly shaped enquiry or draft must not leak into the checklist.
    let previous_documents = std::mem::take(documents);
    // Older immutable Application snapshots did not persist `formType`. They
    // are still safe to hydrate because CRM stores them under `applicationForm`.
    // If a type is explicitly present, however, it must be Application.
    let has_wrong_explicit_type = application
        .get("formType")
        .and_then(Value::as_str)
        .is_some_and(|value| value.to_ascii_lowercase().replace('-', "_") != "application");
    if has_wrong_explicit_type
        || application.get("status").and_then(Value::as_str) != Some("submitted")
    {
        return;
    }

    let schema = application.get("schema").unwrap_or(&Value::Null);
    let Some(sections) = schema
        .get("sections")
        .and_then(Value::as_array)
        .or_else(|| schema.as_array())
    else {
        return;
    };
    let data = application.get("data").unwrap_or(&Value::Null);
    let values = data.get("values").unwrap_or(data);
    let Some(values) = values.as_object() else {
        return;
    };

    let submission_id = application
        .get("submissionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let form_version = application
        .get("formVersion")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());

    for field in sections
        .iter()
        .filter_map(|section| section.get("fields").and_then(Value::as_array))
        .flatten()
    {
        let field_type = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !field_type.eq_ignore_ascii_case("upload")
            && !field_type.eq_ignore_ascii_case("image upload")
        {
            continue;
        }
        let key = field
            .get("key")
            .and_then(Value::as_str)
            .or_else(|| field.get("label").and_then(Value::as_str))
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let config = field.get("documentConfig").and_then(Value::as_object);
        let document_type = config
            .and_then(|config| config.get("documentType"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(key);
        let label = field
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(document_type);
        let required = field.get("required").and_then(Value::as_bool) == Some(true);
        let media = values.get(key).and_then(Value::as_object);
        let valid_media = media.is_some_and(|media| {
            media.get("storage").and_then(Value::as_str) == Some("cloudinary")
                && media
                    .get("secureUrl")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.starts_with("https://"))
                && media
                    .get("publicId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        });

        let existing = previous_documents
            .iter()
            .find(|record| {
                record.source_form_field_key.as_deref() == Some(key)
                    || record.document_type == document_type
            })
            .cloned();
        let mut record = existing.unwrap_or(DocumentRecord {
            document_type: document_type.to_owned(),
            label: Some(label.to_owned()),
            required,
            state: DocumentState::NotSubmitted,
            file_id: None,
            file_name: None,
            content_type: None,
            secure_url: None,
            bytes: None,
            uploaded_at: None,
            source_form_field_key: Some(key.to_owned()),
            source_submission_id: submission_id.clone(),
            source_form_version: form_version,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            expires_at: None,
        });
        record.label = Some(label.to_owned());
        record.required = required;
        record.source_form_field_key = Some(key.to_owned());
        record.source_submission_id = submission_id.clone();
        record.source_form_version = form_version;
        if !valid_media {
            if record.state != DocumentState::Waived {
                record.state = DocumentState::NotSubmitted;
                record.file_id = None;
                record.file_name = None;
                record.content_type = None;
                record.secure_url = None;
                record.bytes = None;
                record.uploaded_at = None;
                record.verified_by = None;
                record.verified_at = None;
                record.rejection_reason = None;
            }
            documents.push(record);
            continue;
        }
        let media = media.expect("validated application media");
        let public_id = media.get("publicId").and_then(Value::as_str);
        let file_changed = record.file_id.as_deref() != public_id;
        if file_changed
            || matches!(
                record.state,
                DocumentState::NotSubmitted | DocumentState::Expired
            )
        {
            record.state = DocumentState::Submitted;
            record.verified_by = None;
            record.verified_at = None;
            record.rejection_reason = None;
        }
        record.file_id = public_id.map(str::to_owned);
        record.file_name = media
            .get("fileName")
            .and_then(Value::as_str)
            .map(str::to_owned);
        record.content_type = media
            .get("contentType")
            .and_then(Value::as_str)
            .map(str::to_owned);
        record.secure_url = media
            .get("secureUrl")
            .and_then(Value::as_str)
            .map(str::to_owned);
        record.bytes = media.get("bytes").and_then(Value::as_u64);
        record.uploaded_at = media
            .get("uploadedAt")
            .and_then(Value::as_str)
            .map(str::to_owned);
        record.source_form_field_key = Some(key.to_owned());
        record.source_submission_id = submission_id.clone();
        record.source_form_version = form_version;
        documents.push(record);
    }
}

#[cfg(test)]
mod application_document_mapping_tests {
    use super::*;
    use crate::domain::default_workflow;
    use serde_json::json;

    fn mapped_trigger(form_type: &str) -> AdmissionTrigger {
        AdmissionTrigger {
            tenant_id: "tenant-local".into(),
            applicant_id: "applicant-1".into(),
            application_id: "application-1".into(),
            admission_id: "admission-1".into(),
            admission_status: "CONFIRMED".into(),
            attributes: json!({
                "applicationForm": {
                    "submissionId": "submission-1",
                    "formId": "form-1",
                    "formType": form_type,
                    "formVersion": 4,
                    "status": "submitted",
                    "schema": { "sections": [{ "fields": [{
                        "key": "certificate_10",
                        "label": "10th Certificate",
                        "type": "Upload",
                        "documentConfig": {
                            "enabled": true,
                            "documentType": "certificate-10"
                        }
                    }] }] },
                    "data": { "values": { "certificate_10": {
                        "storage": "cloudinary",
                        "fileName": "certificate.pdf",
                        "contentType": "application/pdf",
                        "secureUrl": "https://res.cloudinary.com/example/raw/upload/certificate.pdf",
                        "publicId": "supercampus/tenant-local/media/certificate",
                        "bytes": 2048,
                        "uploadedAt": "2026-08-13T10:00:00Z"
                    } } }
                }
            })
            .as_object()
            .cloned()
            .expect("attributes"),
            ..AdmissionTrigger::default()
        }
    }

    #[test]
    fn submitted_application_upload_populates_the_matching_checklist_record() {
        let definition = default_workflow("tenant-local");
        let case = create_case(
            &mapped_trigger("application"),
            &definition,
            CreateCaseOptions {
                id: "case-1".into(),
                now: Utc::now(),
                assigned_to: None,
            },
        );
        let record = case
            .documents
            .iter()
            .find(|record| record.document_type == "certificate-10")
            .expect("10th certificate record");
        assert_eq!(record.state, DocumentState::Submitted);
        assert_eq!(record.file_name.as_deref(), Some("certificate.pdf"));
        assert_eq!(record.source_submission_id.as_deref(), Some("submission-1"));
        assert_eq!(record.source_form_version, Some(4));
    }

    #[test]
    fn similarly_shaped_non_application_form_is_ignored() {
        let definition = default_workflow("tenant-local");
        let case = create_case(
            &mapped_trigger("enquiry"),
            &definition,
            CreateCaseOptions {
                id: "case-1".into(),
                now: Utc::now(),
                assigned_to: None,
            },
        );
        assert!(case.documents.is_empty());
    }

    #[test]
    fn older_application_snapshot_without_form_type_is_still_mapped() {
        let definition = default_workflow("tenant-local");
        let mut trigger = mapped_trigger("application");
        trigger
            .attributes
            .get_mut("applicationForm")
            .and_then(Value::as_object_mut)
            .expect("application snapshot")
            .remove("formType");

        let case = create_case(
            &trigger,
            &definition,
            CreateCaseOptions {
                id: "case-1".into(),
                now: Utc::now(),
                assigned_to: None,
            },
        );

        assert_eq!(case.documents.len(), 1);
        assert_eq!(case.documents[0].document_type, "certificate-10");
    }

    #[test]
    fn legacy_documents_not_declared_by_application_form_are_removed() {
        let definition = default_workflow("tenant-local");
        let trigger = mapped_trigger("application");
        let mut case = create_case(
            &trigger,
            &definition,
            CreateCaseOptions {
                id: "case-1".into(),
                now: Utc::now(),
                assigned_to: None,
            },
        );
        let mut legacy = case.documents[0].clone();
        legacy.document_type = "transfer-certificate".into();
        legacy.label = Some("Transfer Certificate".into());
        legacy.source_form_field_key = None;
        case.documents.push(legacy);

        apply_application_document_mapping(&mut case.documents, &trigger.attributes);

        assert_eq!(case.documents.len(), 1);
        assert_eq!(case.documents[0].document_type, "certificate-10");
    }

    #[test]
    fn workflow_document_facts_survive_without_an_application_form_snapshot() {
        let definition = default_workflow("tenant-local");
        let trigger = mapped_trigger("application");
        let mut case = create_case(
            &trigger,
            &definition,
            CreateCaseOptions {
                id: "case-1".into(),
                now: Utc::now(),
                assigned_to: None,
            },
        );
        case.attributes.remove("applicationForm");
        case.documents[0].state = DocumentState::Verified;

        apply_application_document_mapping(&mut case.documents, &case.attributes);

        assert_eq!(case.documents.len(), 1);
        assert_eq!(case.documents[0].state, DocumentState::Verified);
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
