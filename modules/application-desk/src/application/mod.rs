//! Application services: the orchestration that binds the pure engine to
//! storage.
//!
//! One action is one database transaction. The case row is locked, the engine
//! decides, the effects run inside savepoints, and state + audit + outbox all
//! commit together. Nothing is published until the commit succeeds.

use chrono::Utc;
use serde_json::{Value, json};
use supercampus_database::Database;

use crate::{
    domain::{
        ActionKind, ActionPayload, AdmissionTrigger, CreateCaseOptions, EngineContext,
        OnboardingCase, OnboardingEventName, OnboardingStatus, apply_action, create_case,
        evaluate_intake, summarise_queues,
    },
    infrastructure::postgres::{
        DeskError, DeskSettings, PostgresDeskRepository, PostgresOnboardingServices,
    },
};

/// Who is acting, and what they are allowed to do.
#[derive(Debug, Clone)]
pub struct ActorContext {
    pub user_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}

impl ActorContext {
    /// Exact match, the global `*`, or a `namespace.*` grant.
    ///
    /// The last form is what lets an institution grant the whole desk to a role
    /// without enumerating twelve permissions.
    pub fn has(&self, permission: &str) -> bool {
        self.permissions.iter().any(|granted| {
            granted == "*"
                || granted == permission
                || granted.strip_suffix(".*").is_some_and(|namespace| {
                    permission == namespace
                        || permission
                            .strip_prefix(namespace)
                            .is_some_and(|rest| rest.starts_with('.'))
                })
        })
    }
}

/// Everything the desk screen renders in one payload.
#[derive(Debug, Clone)]
pub struct DeskSnapshot {
    pub definition: Value,
    pub cases: Vec<OnboardingCase>,
    pub audit: Vec<Value>,
    pub events: Vec<Value>,
    pub queues: Value,
}

impl DeskSnapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "definition": self.definition,
            "cases": self.cases,
            "audit": self.audit,
            "events": self.events,
            "queues": self.queues,
        })
    }
}

/// One workflow action against one case.
#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub case_id: String,
    pub action: ActionKind,
    pub reason: Option<String>,
    pub payload: ActionPayload,
}

#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub snapshot: DeskSnapshot,
}

const AUDIT_LIMIT: i64 = 200;
const EVENT_LIMIT: i64 = 200;

#[derive(Clone)]
pub struct ApplicationDeskService {
    repository: PostgresDeskRepository,
}

impl ApplicationDeskService {
    pub fn new(database: Database) -> Self {
        Self {
            repository: PostgresDeskRepository::new(database),
        }
    }

    /// Read the whole desk for one tenant.
    pub async fn snapshot(&self, tenant_slug: &str) -> Result<DeskSnapshot, DeskError> {
        let (tenant_id, mut transaction) = self.repository.begin_tenant(tenant_slug).await?;
        let definition =
            PostgresDeskRepository::active_workflow(&mut transaction, tenant_id, tenant_slug)
                .await?;
        let cases = PostgresDeskRepository::list_cases(&mut transaction, tenant_id).await?;
        let audit =
            PostgresDeskRepository::recent_audit(&mut transaction, tenant_id, AUDIT_LIMIT).await?;
        let events =
            PostgresDeskRepository::recent_events(&mut transaction, tenant_id, EVENT_LIMIT).await?;
        transaction.commit().await?;

        Ok(build_snapshot(&definition, cases, audit, events))
    }

    /// Open a case for a confirmed admission.
    pub async fn open_case(
        &self,
        tenant_slug: &str,
        actor: &ActorContext,
        mut trigger: AdmissionTrigger,
    ) -> Result<(bool, Option<String>, DeskSnapshot), DeskError> {
        trigger.tenant_id = tenant_slug.to_owned();

        let (tenant_id, mut transaction) = self.repository.begin_tenant(tenant_slug).await?;
        let definition =
            PostgresDeskRepository::active_workflow(&mut transaction, tenant_id, tenant_slug)
                .await?;
        let settings = PostgresDeskRepository::settings(&mut transaction, tenant_id).await?;
        let existing = PostgresDeskRepository::list_cases(&mut transaction, tenant_id).await?;

        let decision = evaluate_intake(&trigger, &existing, settings.intake_mode);
        if !decision.create {
            transaction.commit().await?;
            let snapshot = self.snapshot(tenant_slug).await?;
            return Ok((false, Some(decision.reason), snapshot));
        }

        let now = Utc::now();
        let year = trigger
            .academic_year
            .clone()
            .unwrap_or_else(|| now.format("%Y").to_string());
        let id = PostgresDeskRepository::next_case_id(&mut transaction, tenant_id, &year).await?;

        let onboarding = create_case(
            &trigger,
            &definition,
            CreateCaseOptions {
                id,
                now,
                assigned_to: Some(actor.user_id.clone()),
            },
        );

        PostgresDeskRepository::upsert_case(&mut transaction, tenant_id, &onboarding).await?;
        let event = crate::domain::OnboardingEvent {
            name: OnboardingEventName::OnboardingCreated,
            case_id: onboarding.id.clone(),
            tenant_id: onboarding.tenant_id.clone(),
            timestamp: now,
            payload: serde_json::Map::new(),
        };
        PostgresDeskRepository::enqueue_events(&mut transaction, tenant_id, &[event]).await?;
        transaction.commit().await?;

        let snapshot = self.snapshot(tenant_slug).await?;
        Ok((true, None, snapshot))
    }

    /// Apply one workflow action to one case.
    pub async fn act(
        &self,
        tenant_slug: &str,
        actor: &ActorContext,
        request: ActionRequest,
    ) -> Result<ActionOutcome, DeskError> {
        let ActionRequest {
            case_id,
            action,
            reason,
            payload,
        } = request;
        let case_id = case_id.as_str();
        let (tenant_id, mut transaction) = self.repository.begin_tenant(tenant_slug).await?;
        let onboarding =
            PostgresDeskRepository::lock_case(&mut transaction, tenant_id, case_id).await?;
        // The case runs under the workflow version it was opened with.
        let definition = PostgresDeskRepository::pinned_workflow(
            &mut transaction,
            tenant_id,
            tenant_slug,
            &onboarding.workflow_id,
            onboarding.workflow_version,
        )
        .await?;
        let settings: DeskSettings =
            PostgresDeskRepository::settings(&mut transaction, tenant_id).await?;

        let services =
            PostgresOnboardingServices::new(transaction, tenant_id, settings, &actor.user_id);
        let result = {
            let context = EngineContext::new(actor.user_id.clone(), Utc::now(), &services)
                .with_reason(reason)
                .with_payload(payload);
            apply_action(&definition, &onboarding, action, &context).await
        };
        let mut transaction = services.into_transaction();

        // Facts the operator recorded survive a refusal. Approving step 1 of a
        // two-step chain is refused by `approvalsComplete` until step 2 lands —
        // if the refusal discarded the payload, the chain could never complete.
        // The transition itself did not happen, so no audit row and no events.
        let recorded_new_facts = result.case != onboarding;
        let moved = result.ok || result.case.status == OnboardingStatus::Failed;

        if moved || recorded_new_facts {
            PostgresDeskRepository::upsert_case(&mut transaction, tenant_id, &result.case).await?;
        }
        if moved {
            PostgresDeskRepository::insert_audit(&mut transaction, tenant_id, &result.audit)
                .await?;
            PostgresDeskRepository::enqueue_events(&mut transaction, tenant_id, &result.events)
                .await?;
        }
        transaction.commit().await?;

        let snapshot = self.snapshot(tenant_slug).await?;
        Ok(ActionOutcome {
            ok: result.ok,
            error: result.error,
            snapshot,
        })
    }
}

fn build_snapshot(
    definition: &crate::domain::WorkflowDefinition,
    cases: Vec<OnboardingCase>,
    audit: Vec<Value>,
    events: Vec<Value>,
) -> DeskSnapshot {
    let queues = summarise_queues(&cases);
    DeskSnapshot {
        definition: serde_json::to_value(definition).unwrap_or(Value::Null),
        queues: json!(queues),
        cases,
        audit,
        events,
    }
}
