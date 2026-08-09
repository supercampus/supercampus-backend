//! The onboarding workflow engine.
//!
//! Load workflow -> load case -> evaluate conditions -> validate stage
//! -> execute effects -> persist state -> audit -> emit events -> next stage
//!
//! Two properties are deliberate:
//!
//!  - **Deterministic.** The engine never reads the clock or generates ids
//!    itself; `now` and the integration services arrive through [`EngineContext`].
//!    That keeps it unit-testable and keeps retries reproducible.
//!  - **Idempotent.** Every effect is keyed by the onboarding id, so replaying
//!    an action can never create a second student or a second account.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::{
    guards::run_guards,
    types::{
        ApplicantSnapshot, ApprovalRecord, ApprovalState, AuditEntry, DocumentRecord,
        DocumentState, ExceptionKind, FinanceState, IdentityMatchKind, OnboardingCase,
        OnboardingEvent, OnboardingEventName, OnboardingException, OnboardingStage,
        OnboardingStatus,
    },
    workflow::{ActionKind, EffectKind, WorkflowDefinition, evaluate_conditions, project},
};

/// Integration boundary. The desk *requests* work from owning modules; it never
/// writes their tables directly.
#[async_trait]
pub trait OnboardingServices: Send + Sync {
    async fn generate_student_number(
        &self,
        onboarding: &OnboardingCase,
        definition: &WorkflowDefinition,
    ) -> Result<String, ServiceError>;

    async fn create_student(&self, onboarding: &OnboardingCase) -> Result<String, ServiceError>;

    async fn create_user_account(
        &self,
        onboarding: &OnboardingCase,
    ) -> Result<String, ServiceError>;

    async fn provision_access(&self, onboarding: &OnboardingCase) -> Result<(), ServiceError>;

    async fn notify(&self, onboarding: &OnboardingCase, template: &str)
    -> Result<(), ServiceError>;
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ServiceError(pub String);

impl ServiceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Facts an action may carry in alongside the transition.
///
/// The workflow needs a way to *record* what an operator verified — a bare
/// `{action}` can never satisfy the document or approval guards. Each field is
/// optional and only the ones present are applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_match: Option<IdentityMatchKind>,
    /// Document states to merge into the checklist, keyed by document type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finance: Option<FinanceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub academic_year: Option<String>,
    /// Approval step to record against the acting user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_step: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentUpdate {
    #[serde(rename = "type")]
    pub document_type: String,
    pub state: DocumentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
}

pub struct EngineContext<'a> {
    pub actor: String,
    /// Timestamp injected by the caller — the engine stays clock-free.
    pub now: DateTime<Utc>,
    pub reason: Option<String>,
    pub payload: ActionPayload,
    pub services: &'a dyn OnboardingServices,
}

impl<'a> EngineContext<'a> {
    pub fn new(
        actor: impl Into<String>,
        now: DateTime<Utc>,
        services: &'a dyn OnboardingServices,
    ) -> Self {
        Self {
            actor: actor.into(),
            now,
            reason: None,
            payload: ActionPayload::default(),
            services,
        }
    }

    pub fn with_reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }

    pub fn with_payload(mut self, payload: ActionPayload) -> Self {
        self.payload = payload;
        self
    }
}

#[derive(Debug, Clone)]
pub struct TransitionResult {
    pub ok: bool,
    pub case: OnboardingCase,
    pub events: Vec<OnboardingEvent>,
    pub audit: Vec<AuditEntry>,
    pub exception: Option<OnboardingException>,
    /// Populated when the action was refused; the case is returned unchanged.
    pub error: Option<String>,
}

fn stage_event(stage: OnboardingStage) -> Option<OnboardingEventName> {
    Some(match stage {
        OnboardingStage::IdentityVerification => OnboardingEventName::IdentityVerified,
        OnboardingStage::DocumentVerification => OnboardingEventName::DocumentsVerified,
        OnboardingStage::AcademicMapping => OnboardingEventName::AcademicMappingCompleted,
        OnboardingStage::SectionAllocation => OnboardingEventName::SectionAllocated,
        OnboardingStage::FinanceVerification => OnboardingEventName::FinanceVerified,
        OnboardingStage::StudentCreation => OnboardingEventName::StudentCreated,
        OnboardingStage::AccountProvisioning => OnboardingEventName::UserCreated,
        OnboardingStage::AccessProvisioning => OnboardingEventName::AccessProvisioned,
        OnboardingStage::Activation => OnboardingEventName::StudentActivated,
        _ => return None,
    })
}

fn event(
    onboarding: &OnboardingCase,
    name: OnboardingEventName,
    timestamp: DateTime<Utc>,
    payload: Map<String, Value>,
) -> OnboardingEvent {
    OnboardingEvent {
        name,
        case_id: onboarding.id.clone(),
        tenant_id: onboarding.tenant_id.clone(),
        timestamp,
        payload,
    }
}

fn empty_payload() -> Map<String, Value> {
    Map::new()
}

fn audit_entry(
    before: &OnboardingCase,
    after: &OnboardingCase,
    action: &str,
    context: &EngineContext<'_>,
) -> AuditEntry {
    AuditEntry {
        case_id: before.id.clone(),
        actor: context.actor.clone(),
        action: action.to_owned(),
        from_stage: before.stage,
        to_stage: after.stage,
        from_status: before.status,
        to_status: after.status,
        timestamp: context.now,
        reason: context.reason.clone(),
    }
}

fn refuse(onboarding: &OnboardingCase, error: impl Into<String>) -> TransitionResult {
    TransitionResult {
        ok: false,
        case: onboarding.clone(),
        events: Vec::new(),
        audit: Vec::new(),
        exception: None,
        error: Some(error.into()),
    }
}

/// Idempotency key for one effect on one case.
pub fn effect_key(case_id: &str, effect: EffectKind) -> String {
    format!("{case_id}:{}", effect.as_str())
}

/// Apply the operator-supplied facts to the case before the guards run.
///
/// This is what makes `verify` / `assign` / `approve` meaningful: without it a
/// case could never clear the document or approval guards through the API.
fn apply_payload(onboarding: &OnboardingCase, context: &EngineContext<'_>) -> OnboardingCase {
    let payload = &context.payload;
    let mut draft = onboarding.clone();

    if let Some(identity) = payload.identity_match {
        draft.identity_match = Some(identity);
    }
    for update in &payload.documents {
        if let Some(existing) = draft
            .documents
            .iter_mut()
            .find(|record| record.document_type == update.document_type)
        {
            existing.state = update.state;
            existing.file_id = update.file_id.clone().or_else(|| existing.file_id.clone());
            existing.rejection_reason = update.rejection_reason.clone();
            existing.verified_by = Some(context.actor.clone());
            existing.verified_at = Some(context.now.to_rfc3339());
        } else {
            draft.documents.push(DocumentRecord {
                document_type: update.document_type.clone(),
                state: update.state,
                file_id: update.file_id.clone(),
                verified_by: Some(context.actor.clone()),
                verified_at: Some(context.now.to_rfc3339()),
                rejection_reason: update.rejection_reason.clone(),
                expires_at: None,
            });
        }
    }
    if let Some(finance) = payload.finance {
        draft.finance = finance;
    }
    if let Some(section) = payload.section_id.clone() {
        draft.academic.section_id = Some(section);
    }
    if let Some(program) = payload.program_id.clone() {
        draft.academic.program_id = Some(program);
    }
    if let Some(department) = payload.department_id.clone() {
        draft.academic.department_id = Some(department);
    }
    if let Some(batch) = payload.batch_id.clone() {
        draft.academic.batch_id = Some(batch);
    }
    if let Some(year) = payload.academic_year.clone() {
        draft.academic.academic_year = Some(year);
    }
    if let Some(assigned) = payload.assigned_to.clone() {
        draft.assigned_to = Some(assigned);
    }
    if let Some(step) = payload.approval_step {
        if let Some(record) = draft.approvals.iter_mut().find(|entry| entry.step == step) {
            record.state = ApprovalState::Approved;
            record.acted_by = Some(context.actor.clone());
            record.acted_at = Some(context.now.to_rfc3339());
            record.comment = context.reason.clone();
        } else {
            draft.approvals.push(ApprovalRecord {
                step,
                role: String::new(),
                state: ApprovalState::Approved,
                acted_by: Some(context.actor.clone()),
                acted_at: Some(context.now.to_rfc3339()),
                comment: context.reason.clone(),
            });
        }
    }

    draft
}

struct EffectOutcome {
    case: OnboardingCase,
    events: Vec<OnboardingEvent>,
    exception: Option<OnboardingException>,
}

/// Run a stage's effects. Each is skipped when its idempotency key is already
/// present, so a retried transition reuses the original student number, student
/// id and account id rather than minting new ones.
async fn run_effects(
    onboarding: &OnboardingCase,
    definition: &WorkflowDefinition,
    effects: &[EffectKind],
    context: &EngineContext<'_>,
) -> EffectOutcome {
    let mut draft = onboarding.clone();
    let mut events = Vec::new();

    for effect in effects {
        let key = effect_key(&draft.id, *effect);
        if draft.applied_effects.contains_key(&key) {
            continue;
        }

        let outcome: Result<(), ServiceError> = match effect {
            EffectKind::GenerateNumber => {
                match context
                    .services
                    .generate_student_number(&draft, definition)
                    .await
                {
                    Ok(student_number) => {
                        draft.student_number = Some(student_number.clone());
                        draft.applied_effects.insert(key, student_number.clone());
                        let mut payload = empty_payload();
                        payload.insert("studentNumber".into(), json!(student_number));
                        events.push(event(
                            &draft,
                            OnboardingEventName::StudentNumberGenerated,
                            context.now,
                            payload,
                        ));
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            EffectKind::CreateStudent => match context.services.create_student(&draft).await {
                Ok(student_id) => {
                    draft.student_id = Some(student_id.clone());
                    draft.applied_effects.insert(key, student_id);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            EffectKind::CreateUser => match context.services.create_user_account(&draft).await {
                Ok(account_id) => {
                    draft.user_account_id = Some(account_id.clone());
                    draft.applied_effects.insert(key, account_id);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            EffectKind::ProvisionAccess => match context.services.provision_access(&draft).await {
                Ok(()) => {
                    draft.access_provisioned = Some(true);
                    draft.applied_effects.insert(key, "provisioned".into());
                    Ok(())
                }
                Err(error) => Err(error),
            },
            EffectKind::Notify => {
                match context.services.notify(&draft, "onboarding.welcome").await {
                    Ok(()) => {
                        draft.applied_effects.insert(key, "sent".into());
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        };

        if let Err(error) = outcome {
            // The already-completed effects stay recorded, so the retry reuses them.
            draft.status = OnboardingStatus::Failed;
            let mut payload = empty_payload();
            payload.insert("effect".into(), json!(effect.as_str()));
            payload.insert("message".into(), json!(error.0));
            events.push(event(
                &draft,
                OnboardingEventName::OnboardingFailed,
                context.now,
                payload,
            ));
            return EffectOutcome {
                exception: Some(OnboardingException {
                    case_id: draft.id.clone(),
                    kind: ExceptionKind::ProvisioningFailed,
                    message: format!("{} failed: {}", effect.as_str(), error.0),
                    retryable: true,
                }),
                case: draft,
                events,
            };
        }
    }

    EffectOutcome {
        case: draft,
        events,
        exception: None,
    }
}

/// Move a case to a side status without losing where it must resume.
fn side_transition(
    onboarding: &OnboardingCase,
    status: OnboardingStatus,
    event_name: OnboardingEventName,
    context: &EngineContext<'_>,
) -> TransitionResult {
    let mut after = onboarding.clone();
    after.status = status;
    after.resume_stage = onboarding.resume_stage.or(Some(onboarding.stage));
    if status == OnboardingStatus::OnHold {
        after.hold_reason = context.reason.clone();
    }
    if matches!(
        status,
        OnboardingStatus::Rejected | OnboardingStatus::Cancelled
    ) {
        after.rejection_reason = context.reason.clone();
    }
    after.updated_at = context.now;

    let mut payload = empty_payload();
    if let Some(reason) = context.reason.clone() {
        payload.insert("reason".into(), json!(reason));
    }

    TransitionResult {
        ok: true,
        events: vec![event(&after, event_name, context.now, payload)],
        audit: vec![audit_entry(onboarding, &after, status.as_str(), context)],
        case: after,
        exception: None,
        error: None,
    }
}

/// Apply an action to a case.
///
/// `advance`, `verify` and `approve` are forward moves: they validate the
/// current stage's conditions and guards, run its effects, then hand off to the
/// next enabled stage. Everything else is a lifecycle action.
pub async fn apply_action(
    definition: &WorkflowDefinition,
    onboarding: &OnboardingCase,
    action: ActionKind,
    context: &EngineContext<'_>,
) -> TransitionResult {
    if onboarding.status.is_terminal() {
        return refuse(
            onboarding,
            format!(
                "Case is {} and can no longer transition",
                onboarding.status.as_str()
            ),
        );
    }

    match action {
        ActionKind::Hold => {
            return side_transition(
                onboarding,
                OnboardingStatus::OnHold,
                OnboardingEventName::OnboardingHeld,
                context,
            );
        }
        ActionKind::Return => {
            return side_transition(
                onboarding,
                OnboardingStatus::Returned,
                OnboardingEventName::OnboardingReturned,
                context,
            );
        }
        ActionKind::Reject => {
            return side_transition(
                onboarding,
                OnboardingStatus::Rejected,
                OnboardingEventName::OnboardingRejected,
                context,
            );
        }
        ActionKind::Cancel => {
            return side_transition(
                onboarding,
                OnboardingStatus::Cancelled,
                OnboardingEventName::OnboardingRejected,
                context,
            );
        }
        ActionKind::Withdraw => {
            return side_transition(
                onboarding,
                OnboardingStatus::Withdrawn,
                OnboardingEventName::OnboardingRejected,
                context,
            );
        }
        ActionKind::Expire => {
            return side_transition(
                onboarding,
                OnboardingStatus::Expired,
                OnboardingEventName::OnboardingFailed,
                context,
            );
        }
        ActionKind::Resume => {
            if !matches!(
                onboarding.status,
                OnboardingStatus::OnHold | OnboardingStatus::Returned
            ) {
                return refuse(onboarding, "Only held or returned cases can be resumed");
            }
            // Resume from the last valid stage rather than restarting.
            let mut after = onboarding.clone();
            after.status = OnboardingStatus::Active;
            after.stage = onboarding.resume_stage.unwrap_or(onboarding.stage);
            after.resume_stage = None;
            after.hold_reason = None;
            after.updated_at = context.now;
            return TransitionResult {
                ok: true,
                events: Vec::new(),
                audit: vec![audit_entry(onboarding, &after, "resume", context)],
                case: after,
                exception: None,
                error: None,
            };
        }
        ActionKind::Advance | ActionKind::Verify | ActionKind::Approve => {}
    }

    // -- forward movement -----------------------------------------------------
    if onboarding.status != OnboardingStatus::Active {
        return refuse(
            onboarding,
            format!(
                "Case is {}; resume it before advancing",
                onboarding.status.as_str()
            ),
        );
    }

    // Record what the operator supplied, then judge the case as it now stands.
    let staged = apply_payload(onboarding, context);

    let Some(current) = definition.stage(staged.stage) else {
        return refuse(
            &staged,
            format!(
                "Stage {} is not in workflow {}",
                staged.stage.as_str(),
                definition.id
            ),
        );
    };
    if !current.enabled {
        return refuse(&staged, format!("Stage {} is disabled", current.label));
    }

    let projection = project(&staged);
    if !evaluate_conditions(&projection, &current.conditions) {
        return refuse(
            &staged,
            format!("Stage conditions for {} are not satisfied", current.label),
        );
    }

    let guard = run_guards(&staged, definition, &current.guards);
    if !guard.ok {
        return refuse(
            &staged,
            guard.reason.unwrap_or_else(|| "Stage guards failed".into()),
        );
    }

    let applicable = definition.transitions.iter().find(|transition| {
        transition.from == staged.stage
            && transition.action == action
            && evaluate_conditions(&projection, &transition.when)
            && run_guards(&staged, definition, &transition.guards).ok
    });

    let Some(target) = applicable
        .map(|transition| transition.to)
        .or_else(|| definition.next_stage(staged.stage))
    else {
        return refuse(
            &staged,
            format!("No transition available from {}", current.label),
        );
    };

    let effects = current.effects.clone();
    let outcome = run_effects(&staged, definition, &effects, context).await;

    if let Some(exception) = outcome.exception {
        let mut failed = outcome.case;
        failed.updated_at = context.now;
        let audit = vec![audit_entry(
            &staged,
            &failed,
            &format!("{}:failed", action.as_str()),
            context,
        )];
        return TransitionResult {
            ok: false,
            error: Some(exception.message.clone()),
            exception: Some(exception),
            case: failed,
            events: outcome.events,
            audit,
        };
    }

    let completed = target == OnboardingStage::Completed;
    let mut after = outcome.case;
    after.stage = target;
    after.status = if completed {
        OnboardingStatus::Completed
    } else {
        OnboardingStatus::Active
    };
    after.updated_at = context.now;
    if completed {
        after.completed_at = Some(context.now);
    }

    let mut events = outcome.events;
    if let Some(name) = stage_event(staged.stage) {
        events.push(event(&after, name, context.now, empty_payload()));
    }
    if completed {
        let mut payload = empty_payload();
        payload.insert("studentId".into(), json!(after.student_id));
        payload.insert("studentNumber".into(), json!(after.student_number));
        events.push(event(
            &after,
            OnboardingEventName::OnboardingCompleted,
            context.now,
            payload,
        ));
    }

    let audit = vec![audit_entry(&staged, &after, action.as_str(), context)];
    TransitionResult {
        ok: true,
        case: after,
        events,
        audit,
        exception: None,
        error: None,
    }
}

/// Snapshot helper used by intake when seeding contact facts.
pub fn snapshot_from(
    full_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
) -> ApplicantSnapshot {
    ApplicantSnapshot {
        full_name,
        email,
        phone,
        guardian_name: None,
        guardian_email: None,
    }
}
