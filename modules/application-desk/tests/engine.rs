//! Rust port of `supercampus-web/modules/application-desk/src/tests/engine.test.ts`.
//!
//! The TypeScript suite is the acceptance checklist; every test name below
//! mirrors one there, plus the extra cases the Rust engine must prove (a retry
//! does not double-create, and a failed integration reuses its number).

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use supercampus_application_desk::domain::{
    ActionKind, ApprovalState, DocumentState, EngineContext, ExceptionKind, FinanceState,
    IdentityMatchKind, NumberToken, OnboardingCase, OnboardingServices, OnboardingStage,
    OnboardingStatus, ServiceError, StudentNumberFormat, StudentNumberInput, WorkflowDefinition,
    apply_action, create_case, default_workflow, evaluate_intake, format_student_number,
    intake::{AdmissionTrigger, CreateCaseOptions, IntakeTriggerMode},
    queue_of, sequence_scope, summarise_queues,
};

const TENANT: &str = "tenant-1";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 8, 10, 0, 0).unwrap()
}

fn trigger() -> AdmissionTrigger {
    AdmissionTrigger {
        tenant_id: TENANT.into(),
        applicant_id: "APP-2026-00891".into(),
        application_id: "APL-2026-00891".into(),
        admission_id: "ADM-2026-00452".into(),
        admission_status: "CONFIRMED".into(),
        academic_year: Some("2026".into()),
        program_id: Some("prog-btech-cse".into()),
        department_id: Some("dept-cse".into()),
        campus_id: Some("campus-main".into()),
        batch_id: Some("batch-2026".into()),
        ..Default::default()
    }
}

/// Counting services so tests can assert effects ran exactly once.
#[derive(Default)]
struct Calls {
    number: AtomicUsize,
    student: AtomicUsize,
    user: AtomicUsize,
    access: AtomicUsize,
    notify: AtomicUsize,
}

impl Calls {
    fn get(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }
}

struct CountingServices {
    calls: Arc<Calls>,
    /// When set, `create_student` fails with this message.
    student_failure: Option<String>,
}

impl CountingServices {
    fn new() -> (Self, Arc<Calls>) {
        let calls = Arc::new(Calls::default());
        (
            Self {
                calls: Arc::clone(&calls),
                student_failure: None,
            },
            calls,
        )
    }

    fn failing_student(message: &str) -> (Self, Arc<Calls>) {
        let (mut services, calls) = Self::new();
        services.student_failure = Some(message.into());
        (services, calls)
    }
}

#[async_trait]
impl OnboardingServices for CountingServices {
    async fn generate_student_number(
        &self,
        _: &OnboardingCase,
        _: &WorkflowDefinition,
    ) -> Result<String, ServiceError> {
        let next = self.calls.number.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("2026CSE{next:03}"))
    }

    async fn create_student(&self, _: &OnboardingCase) -> Result<String, ServiceError> {
        if let Some(message) = &self.student_failure {
            return Err(ServiceError::new(message.clone()));
        }
        let next = self.calls.student.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("STU-{next}"))
    }

    async fn create_user_account(&self, _: &OnboardingCase) -> Result<String, ServiceError> {
        let next = self.calls.user.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("USR-{next}"))
    }

    async fn provision_access(&self, _: &OnboardingCase) -> Result<(), ServiceError> {
        self.calls.access.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn notify(&self, _: &OnboardingCase, _: &str) -> Result<(), ServiceError> {
        self.calls.notify.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A case with every guard satisfied, parked at `stage`.
fn ready_case(stage: OnboardingStage) -> OnboardingCase {
    let definition = default_workflow(TENANT);
    let mut base = create_case(
        &trigger(),
        &definition,
        CreateCaseOptions {
            id: "ONB-2026-000145".into(),
            now: now(),
            assigned_to: None,
        },
    );
    base.stage = stage;
    base.identity_match = Some(IdentityMatchKind::NoMatch);
    for document in &mut base.documents {
        document.state = DocumentState::Verified;
    }
    base.academic.section_id = Some("sec-a".into());
    base.finance = FinanceState::Cleared;
    for approval in &mut base.approvals {
        approval.state = ApprovalState::Approved;
    }
    base
}

// -- intake -----------------------------------------------------------------

#[test]
fn intake_refuses_admissions_that_are_not_confirmed() {
    let mut pending = trigger();
    pending.admission_status = "PENDING".into();
    let decision = evaluate_intake(&pending, &[], IntakeTriggerMode::OnConfirmed);
    assert!(!decision.create);
    assert!(decision.reason.contains("not CONFIRMED"));
}

#[test]
fn intake_blocks_a_duplicate_applicant_but_allows_readmission_after_a_closed_case() {
    let definition = default_workflow(TENANT);
    let existing = create_case(
        &trigger(),
        &definition,
        CreateCaseOptions {
            id: "ONB-1".into(),
            now: now(),
            assigned_to: None,
        },
    );

    let blocked = evaluate_intake(
        &trigger(),
        std::slice::from_ref(&existing),
        IntakeTriggerMode::OnConfirmed,
    );
    assert!(!blocked.create);
    assert_eq!(blocked.duplicate_of.as_deref(), Some("ONB-1"));

    let mut withdrawn = existing;
    withdrawn.status = OnboardingStatus::Withdrawn;
    let reopened = evaluate_intake(&trigger(), &[withdrawn], IntakeTriggerMode::OnConfirmed);
    assert!(
        reopened.create,
        "a withdrawn case must not block re-admission"
    );
}

#[test]
fn intake_blocks_a_duplicate_crm_lead_even_when_external_ids_differ() {
    let lead_id = uuid::Uuid::new_v4();
    let definition = default_workflow(TENANT);
    let mut original = trigger();
    original.crm_lead_id = Some(lead_id);
    let existing = create_case(
        &original,
        &definition,
        CreateCaseOptions {
            id: "ONB-CRM-1".into(),
            now: now(),
            assigned_to: None,
        },
    );
    let mut replay = trigger();
    replay.applicant_id = "different-applicant-id".into();
    replay.application_id = "different-application-id".into();
    replay.admission_id = "different-admission-id".into();
    replay.crm_lead_id = Some(lead_id);

    let decision = evaluate_intake(&replay, &[existing], IntakeTriggerMode::OnConfirmed);
    assert!(!decision.create);
    assert_eq!(decision.duplicate_of.as_deref(), Some("ONB-CRM-1"));
}

#[test]
fn intake_honours_the_fee_paid_trigger_mode() {
    assert!(!evaluate_intake(&trigger(), &[], IntakeTriggerMode::OnFeePaid).create);
    let mut paid = trigger();
    paid.fee_paid = true;
    assert!(evaluate_intake(&paid, &[], IntakeTriggerMode::OnFeePaid).create);
}

// -- guards -----------------------------------------------------------------

#[tokio::test]
async fn a_duplicate_identity_hard_blocks_the_workflow() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let mut onboarding = ready_case(OnboardingStage::IdentityVerification);
    onboarding.identity_match = Some(IdentityMatchKind::Duplicate);

    let context = EngineContext::new("officer-1", now(), &services);
    let result = apply_action(&definition, &onboarding, ActionKind::Advance, &context).await;

    assert!(!result.ok);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains("duplicate")
    );
    assert_eq!(
        result.case.stage,
        OnboardingStage::IdentityVerification,
        "a refused action must not move the case"
    );
}

#[tokio::test]
async fn document_verification_reports_every_outstanding_mandatory_document_at_once() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let mut onboarding = ready_case(OnboardingStage::DocumentVerification);
    for document in &mut onboarding.documents {
        if document.document_type == "photo" || document.document_type == "identity-proof" {
            document.state = DocumentState::NotSubmitted;
        }
    }

    let context = EngineContext::new("officer-1", now(), &services);
    let result = apply_action(&definition, &onboarding, ActionKind::Advance, &context).await;

    assert!(!result.ok);
    let error = result.error.unwrap_or_default();
    assert!(error.contains("Identity Proof"), "got: {error}");
    assert!(error.contains("Passport Photo"), "got: {error}");
}

#[tokio::test]
async fn a_waived_document_satisfies_the_checklist() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let mut onboarding = ready_case(OnboardingStage::DocumentVerification);
    for document in &mut onboarding.documents {
        if document.document_type == "transfer-certificate" {
            document.state = DocumentState::Waived;
        }
    }

    let context = EngineContext::new("officer-1", now(), &services);
    let result = apply_action(&definition, &onboarding, ActionKind::Advance, &context).await;
    assert!(result.ok, "error: {:?}", result.error);
}

#[tokio::test]
async fn a_finance_hold_blocks_but_a_not_required_fee_structure_does_not() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();

    let mut held = ready_case(OnboardingStage::FinanceVerification);
    held.finance = FinanceState::Hold;
    let context = EngineContext::new("officer-1", now(), &services);
    let blocked = apply_action(&definition, &held, ActionKind::Advance, &context).await;
    assert!(!blocked.ok);

    let mut exempt = ready_case(OnboardingStage::FinanceVerification);
    exempt.finance = FinanceState::NotRequired;
    let result = apply_action(&definition, &exempt, ActionKind::Advance, &context).await;
    assert!(result.ok, "error: {:?}", result.error);
}

#[tokio::test]
async fn approval_chain_must_be_fully_approved_before_student_creation() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let mut onboarding = ready_case(OnboardingStage::Approval);
    for approval in &mut onboarding.approvals {
        if approval.step == 2 {
            approval.state = ApprovalState::Pending;
        }
    }

    let context = EngineContext::new("officer-1", now(), &services);
    let result = apply_action(&definition, &onboarding, ActionKind::Advance, &context).await;

    assert!(!result.ok);
    assert!(
        result.error.unwrap_or_default().contains("registrar"),
        "the blocking role must be named"
    );
}

// -- effects and idempotency -----------------------------------------------

#[tokio::test]
async fn student_creation_generates_a_number_and_a_student_exactly_once_on_retry() {
    let definition = default_workflow(TENANT);
    let (services, calls) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);

    let first = apply_action(
        &definition,
        &ready_case(OnboardingStage::StudentCreation),
        ActionKind::Advance,
        &context,
    )
    .await;
    assert!(first.ok, "error: {:?}", first.error);
    assert_eq!(first.case.student_number.as_deref(), Some("2026CSE001"));
    assert_eq!(first.case.student_id.as_deref(), Some("STU-1"));

    // Replaying the same transition on the produced case must not mint again.
    let mut replayed = first.case.clone();
    replayed.stage = OnboardingStage::StudentCreation;
    let replay = apply_action(&definition, &replayed, ActionKind::Advance, &context).await;

    assert!(replay.ok);
    assert_eq!(
        replay.case.student_number.as_deref(),
        Some("2026CSE001"),
        "student number must be stable across retries"
    );
    assert_eq!(replay.case.student_id.as_deref(), Some("STU-1"));
    assert_eq!(
        Calls::get(&calls.number),
        1,
        "number generation must run once"
    );
    assert_eq!(
        Calls::get(&calls.student),
        1,
        "student creation must run once"
    );
}

#[tokio::test]
async fn a_failing_integration_marks_the_case_failed_and_raises_a_retryable_exception() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::failing_student("student service unavailable");
    let context = EngineContext::new("officer-1", now(), &services);

    let result = apply_action(
        &definition,
        &ready_case(OnboardingStage::StudentCreation),
        ActionKind::Advance,
        &context,
    )
    .await;

    assert!(!result.ok);
    assert_eq!(result.case.status, OnboardingStatus::Failed);
    let exception = result.exception.expect("an exception must be raised");
    assert_eq!(exception.kind, ExceptionKind::ProvisioningFailed);
    assert!(exception.retryable);
    // The number was already allocated, so a retry must reuse it, not burn another.
    assert_eq!(result.case.student_number.as_deref(), Some("2026CSE001"));
    assert!(
        result
            .events
            .iter()
            .any(|entry| entry.name.as_str() == "OnboardingFailed")
    );
}

#[tokio::test]
async fn a_retried_failed_case_reuses_its_number_and_creates_one_student() {
    let definition = default_workflow(TENANT);

    // First attempt: the number is minted, then the student service falls over.
    let (failing, _) = CountingServices::failing_student("student service unavailable");
    let failed = {
        let context = EngineContext::new("officer-1", now(), &failing);
        apply_action(
            &definition,
            &ready_case(OnboardingStage::StudentCreation),
            ActionKind::Advance,
            &context,
        )
        .await
    };
    assert_eq!(failed.case.status, OnboardingStatus::Failed);
    let minted = failed.case.student_number.clone();

    // Retry against healthy services, from the recorded FAILED case.
    let (healthy, calls) = CountingServices::new();
    let mut retry_case = failed.case;
    retry_case.status = OnboardingStatus::Active;
    let context = EngineContext::new("officer-1", now(), &healthy);
    let retried = apply_action(&definition, &retry_case, ActionKind::Advance, &context).await;

    assert!(retried.ok, "error: {:?}", retried.error);
    assert_eq!(
        retried.case.student_number, minted,
        "the retry must reuse the number allocated before the failure"
    );
    assert_eq!(
        Calls::get(&calls.number),
        0,
        "a recorded generate_number effect must not run again"
    );
    assert_eq!(
        Calls::get(&calls.student),
        1,
        "exactly one student is created across the failure and the retry"
    );
}

// -- lifecycle --------------------------------------------------------------

#[tokio::test]
async fn hold_records_the_resume_stage_and_resume_returns_to_it_not_to_the_start() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let onboarding = ready_case(OnboardingStage::AcademicMapping);

    let hold_context = EngineContext::new("officer-1", now(), &services)
        .with_reason(Some("Transfer Certificate pending".into()));
    let held = apply_action(&definition, &onboarding, ActionKind::Hold, &hold_context).await;

    assert_eq!(held.case.status, OnboardingStatus::OnHold);
    assert_eq!(
        held.case.resume_stage,
        Some(OnboardingStage::AcademicMapping)
    );
    assert_eq!(
        held.case.hold_reason.as_deref(),
        Some("Transfer Certificate pending")
    );

    let context = EngineContext::new("officer-1", now(), &services);
    let resumed = apply_action(&definition, &held.case, ActionKind::Resume, &context).await;

    assert_eq!(resumed.case.status, OnboardingStatus::Active);
    assert_eq!(resumed.case.stage, OnboardingStage::AcademicMapping);
    assert_eq!(resumed.case.resume_stage, None);
}

#[tokio::test]
async fn a_held_case_cannot_advance_until_it_is_resumed() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);

    let held = apply_action(
        &definition,
        &ready_case(OnboardingStage::AcademicMapping),
        ActionKind::Hold,
        &context,
    )
    .await;
    let result = apply_action(&definition, &held.case, ActionKind::Advance, &context).await;

    assert!(!result.ok);
    assert!(
        result
            .error
            .unwrap_or_default()
            .contains("resume it before advancing")
    );
}

#[tokio::test]
async fn terminal_statuses_refuse_every_further_transition() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);

    let rejected = apply_action(
        &definition,
        &ready_case(OnboardingStage::Approval),
        ActionKind::Reject,
        &context,
    )
    .await;
    assert_eq!(rejected.case.status, OnboardingStatus::Rejected);

    for action in [ActionKind::Advance, ActionKind::Resume, ActionKind::Hold] {
        let result = apply_action(&definition, &rejected.case, action, &context).await;
        assert!(
            !result.ok,
            "{} must be refused on a rejected case",
            action.as_str()
        );
    }
}

#[tokio::test]
async fn rejected_cancelled_and_withdrawn_stay_distinct_states() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);
    let base = ready_case(OnboardingStage::Approval);

    let rejected = apply_action(&definition, &base, ActionKind::Reject, &context).await;
    let cancelled = apply_action(&definition, &base, ActionKind::Cancel, &context).await;
    let withdrawn = apply_action(&definition, &base, ActionKind::Withdraw, &context).await;
    let expired = apply_action(&definition, &base, ActionKind::Expire, &context).await;

    assert_eq!(rejected.case.status, OnboardingStatus::Rejected);
    assert_eq!(cancelled.case.status, OnboardingStatus::Cancelled);
    assert_eq!(withdrawn.case.status, OnboardingStatus::Withdrawn);
    assert_eq!(expired.case.status, OnboardingStatus::Expired);
}

// -- traversal --------------------------------------------------------------

#[tokio::test]
async fn a_disabled_stage_is_skipped_without_rewiring_transitions() {
    let mut definition = default_workflow(TENANT);
    for stage in &mut definition.stages {
        if stage.id == OnboardingStage::SectionAllocation {
            stage.enabled = false;
        }
    }
    let (services, _) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);

    let result = apply_action(
        &definition,
        &ready_case(OnboardingStage::AcademicMapping),
        ActionKind::Advance,
        &context,
    )
    .await;

    assert!(result.ok, "error: {:?}", result.error);
    assert_eq!(
        result.case.stage,
        OnboardingStage::FinanceVerification,
        "disabled stage must be skipped"
    );
}

#[tokio::test]
async fn the_full_happy_path_reaches_completed_with_a_student_account_and_access() {
    let definition = default_workflow(TENANT);
    let (services, calls) = CountingServices::new();
    let context = EngineContext::new("officer-1", now(), &services);

    let mut onboarding = ready_case(OnboardingStage::DataReview);
    let mut seen: Vec<String> = Vec::new();

    let mut step = 0;
    while step < 20 && onboarding.status == OnboardingStatus::Active {
        let result = apply_action(&definition, &onboarding, ActionKind::Advance, &context).await;
        assert!(
            result.ok,
            "stage {} failed: {:?}",
            onboarding.stage.as_str(),
            result.error
        );
        seen.extend(
            result
                .events
                .iter()
                .map(|entry| entry.name.as_str().to_owned()),
        );
        onboarding = result.case;
        step += 1;
    }

    assert_eq!(onboarding.status, OnboardingStatus::Completed);
    assert_eq!(onboarding.stage, OnboardingStage::Completed);
    assert!(
        onboarding.student_number.is_some(),
        "a student number must be assigned"
    );
    assert!(
        onboarding.student_id.is_some(),
        "a Student Master must exist"
    );
    assert!(
        onboarding.user_account_id.is_some(),
        "a user account must exist"
    );
    assert_eq!(onboarding.access_provisioned, Some(true));
    assert_eq!(onboarding.completed_at, Some(now()));

    for expected in [
        "StudentCreated",
        "UserCreated",
        "AccessProvisioned",
        "OnboardingCompleted",
    ] {
        assert!(
            seen.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
    assert_eq!(Calls::get(&calls.student), 1);
    assert_eq!(Calls::get(&calls.user), 1);
}

#[tokio::test]
async fn every_transition_produces_an_audit_entry_with_both_endpoints() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();
    let context =
        EngineContext::new("officer-1", now(), &services).with_reason(Some("reviewed".into()));

    let result = apply_action(
        &definition,
        &ready_case(OnboardingStage::DataReview),
        ActionKind::Advance,
        &context,
    )
    .await;

    let entry = result
        .audit
        .first()
        .expect("an audit entry must be written");
    assert_eq!(entry.actor, "officer-1");
    assert_eq!(entry.from_stage, OnboardingStage::DataReview);
    assert_eq!(entry.to_stage, OnboardingStage::IdentityVerification);
    assert_eq!(entry.reason.as_deref(), Some("reviewed"));
    assert_eq!(entry.timestamp, now());
}

// -- action payload ---------------------------------------------------------

#[tokio::test]
async fn verifying_with_a_payload_records_documents_and_clears_the_guard() {
    let definition = default_workflow(TENANT);
    let (services, _) = CountingServices::new();

    // Start from a case whose mandatory documents are all outstanding.
    let mut onboarding = ready_case(OnboardingStage::DocumentVerification);
    for document in &mut onboarding.documents {
        document.state = DocumentState::NotSubmitted;
    }

    let payload = serde_json::from_value::<supercampus_application_desk::domain::ActionPayload>(
        serde_json::json!({
            "documents": definition
                .document_checklist
                .iter()
                .filter(|requirement| requirement.required)
                .map(|requirement| serde_json::json!({
                    "type": requirement.document_type,
                    "state": "VERIFIED",
                }))
                .collect::<Vec<_>>(),
        }),
    )
    .expect("payload must deserialize");

    let context = EngineContext::new("officer-1", now(), &services).with_payload(payload);
    let result = apply_action(&definition, &onboarding, ActionKind::Verify, &context).await;

    assert!(result.ok, "error: {:?}", result.error);
    assert_eq!(result.case.stage, OnboardingStage::AcademicMapping);
    assert!(
        result
            .case
            .documents
            .iter()
            .all(
                |document| document.verified_by.as_deref() == Some("officer-1")
                    || document.state == DocumentState::NotSubmitted
            )
    );
}

// -- projections and numbering ---------------------------------------------

#[test]
fn queue_projection_buckets_cases_by_stage_and_status() {
    assert_eq!(
        queue_of(&ready_case(OnboardingStage::DocumentVerification)).as_str(),
        "documentsPending"
    );
    assert_eq!(
        queue_of(&ready_case(OnboardingStage::Approval)).as_str(),
        "approvalPending"
    );

    let mut held = ready_case(OnboardingStage::Approval);
    held.status = OnboardingStatus::OnHold;
    assert_eq!(queue_of(&held).as_str(), "onHold");

    let mut completed = ready_case(OnboardingStage::Approval);
    completed.status = OnboardingStatus::Completed;
    assert_eq!(queue_of(&completed).as_str(), "activated");

    let mut failed = ready_case(OnboardingStage::Approval);
    failed.status = OnboardingStatus::Failed;
    assert_eq!(queue_of(&failed).as_str(), "failed");

    let counts = summarise_queues(&[
        ready_case(OnboardingStage::DocumentVerification),
        ready_case(OnboardingStage::Approval),
        held,
    ]);
    assert_eq!(counts.get("documentsPending"), Some(&1));
    assert_eq!(counts.get("approvalPending"), Some(&1));
    assert_eq!(counts.get("onHold"), Some(&1));
}

#[test]
fn student_number_format_is_configuration_not_code() {
    assert_eq!(
        format_student_number(
            &StudentNumberInput {
                year: "2026".into(),
                department_code: Some("CSE".into()),
                program_code: None,
                sequence: 1,
            },
            &StudentNumberFormat::default(),
        ),
        "2026CSE001"
    );

    assert_eq!(
        format_student_number(
            &StudentNumberInput {
                year: "26".into(),
                department_code: Some("CS".into()),
                program_code: None,
                sequence: 1,
            },
            &StudentNumberFormat {
                pattern: vec![
                    NumberToken::Prefix,
                    NumberToken::Year,
                    NumberToken::Department,
                    NumberToken::Sequence,
                ],
                prefix: Some("SC".into()),
                separator: Some("/".into()),
                sequence_width: 3,
            },
        ),
        "SC/26/CS/001"
    );
}

#[test]
fn sequence_scope_restarts_per_tenant_year_and_department() {
    assert_eq!(sequence_scope(TENANT, "2026", "CSE"), "tenant-1:2026:CSE");
    assert_ne!(
        sequence_scope(TENANT, "2026", "CSE"),
        sequence_scope(TENANT, "2026", "MEC"),
        "a different department must be a different scope"
    );
    assert_eq!(sequence_scope(TENANT, "2026", ""), "tenant-1:2026:GEN");
}
