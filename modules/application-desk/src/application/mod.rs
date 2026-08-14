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
        ActionKind, ActionPayload, AdmissionTrigger, AuditEntry, CreateCaseOptions, EngineContext,
        OnboardingCase, OnboardingEventName, OnboardingStage, OnboardingStatus, apply_action,
        apply_application_document_mapping, create_case, engine::ApplicationFormUpdate,
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
    pub application_form: Option<Value>,
    pub desk_form: Option<Value>,
    pub cases: Vec<OnboardingCase>,
    pub audit: Vec<Value>,
    pub events: Vec<Value>,
    pub queues: Value,
}

impl DeskSnapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "definition": self.definition,
            "applicationForm": self.application_form,
            "deskForm": self.desk_form,
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
        let application_form =
            PostgresDeskRepository::published_application_form(&mut transaction, tenant_id).await?;
        let desk_form =
            PostgresDeskRepository::published_desk_form(&mut transaction, tenant_id).await?;
        let mut cases = PostgresDeskRepository::list_cases(&mut transaction, tenant_id).await?;
        for onboarding in &mut cases {
            hydrate_matching_application_schema(
                &mut onboarding.attributes,
                application_form.as_ref(),
            );
            apply_application_document_mapping(&mut onboarding.documents, &onboarding.attributes);
        }
        if application_form.is_some() {
            for onboarding in &mut cases {
                if onboarding.stage == crate::domain::OnboardingStage::DataReview {
                    onboarding
                        .attributes
                        .insert("applicationFormRequired".into(), json!(true));
                }
            }
        }
        let audit =
            PostgresDeskRepository::recent_audit(&mut transaction, tenant_id, AUDIT_LIMIT).await?;
        let events =
            PostgresDeskRepository::recent_events(&mut transaction, tenant_id, EVENT_LIMIT).await?;
        transaction.commit().await?;

        Ok(build_snapshot(
            &definition,
            application_form,
            desk_form,
            cases,
            audit,
            events,
        ))
    }

    /// Open a review case for a confirmed admission.
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
        let application_form =
            PostgresDeskRepository::published_application_form(&mut transaction, tenant_id).await?;
        if let Some(form) = application_form.as_ref() {
            trigger
                .attributes
                .insert("applicationFormRequired".into(), json!(true));
            if let Some(id) = form.get("id") {
                trigger
                    .attributes
                    .insert("applicationFormId".into(), id.clone());
            }
        }
        hydrate_matching_application_schema(&mut trigger.attributes, application_form.as_ref());
        let existing = PostgresDeskRepository::list_cases(&mut transaction, tenant_id).await?;

        let decision = evaluate_intake(&trigger, &existing, settings.intake_mode);
        if !decision.create {
            // Offer acceptance is a later fact for the same application, not a
            // reason to create a second case. Record it once on the existing
            // authoritative case so the conversion remains traceable.
            let offer_accepted = trigger
                .attributes
                .get("handoffReason")
                .and_then(Value::as_str)
                == Some("offer_accepted");
            if offer_accepted && let Some(case_id) = decision.duplicate_of.as_deref() {
                let mut onboarding =
                    PostgresDeskRepository::lock_case(&mut transaction, tenant_id, case_id).await?;
                let already_recorded = onboarding.attributes.contains_key("offerAcceptedAt");
                let now = Utc::now();
                for (key, value) in &trigger.attributes {
                    onboarding.attributes.insert(key.clone(), value.clone());
                }
                apply_application_document_mapping(
                    &mut onboarding.documents,
                    &onboarding.attributes,
                );
                if !already_recorded {
                    onboarding
                        .attributes
                        .insert("offerAcceptedAt".into(), json!(now));
                    onboarding
                        .attributes
                        .insert("offerAcceptedBy".into(), json!(actor.user_id));
                    PostgresDeskRepository::insert_audit(
                        &mut transaction,
                        tenant_id,
                        &[AuditEntry {
                            case_id: onboarding.id.clone(),
                            actor: actor.user_id.clone(),
                            action: "offer_accepted_handoff".into(),
                            from_stage: onboarding.stage,
                            to_stage: onboarding.stage,
                            from_status: onboarding.status,
                            to_status: onboarding.status,
                            timestamp: now,
                            reason: Some(
                                "CRM offer acceptance linked to the existing application case"
                                    .into(),
                            ),
                        }],
                    )
                    .await?;
                }
                onboarding.updated_at = now;
                PostgresDeskRepository::upsert_case(&mut transaction, tenant_id, &onboarding)
                    .await?;
            }
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
        PostgresDeskRepository::insert_audit(
            &mut transaction,
            tenant_id,
            &[AuditEntry {
                case_id: onboarding.id.clone(),
                actor: actor.user_id.clone(),
                action: "offer_accepted_handoff".into(),
                from_stage: OnboardingStage::New,
                to_stage: OnboardingStage::DataReview,
                from_status: OnboardingStatus::Active,
                to_status: OnboardingStatus::Active,
                timestamp: now,
                reason: Some(
                    "CRM offer accepted; Admission Desk review case created or reused".into(),
                ),
            }],
        )
        .await?;
        let event = crate::domain::OnboardingEvent {
            name: OnboardingEventName::OnboardingCreated,
            case_id: onboarding.id.clone(),
            tenant_id: onboarding.tenant_id.clone(),
            timestamp: now,
            payload: json!({
                "trigger": "offer_accepted",
                "crmLeadId": onboarding.crm_lead_id,
                "applicationId": onboarding.application_id,
                "offerId": onboarding.admission_id,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
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
        let mut onboarding =
            PostgresDeskRepository::lock_case(&mut transaction, tenant_id, case_id).await?;
        let published_application =
            PostgresDeskRepository::published_application_form(&mut transaction, tenant_id).await?;
        hydrate_matching_application_schema(
            &mut onboarding.attributes,
            published_application.as_ref(),
        );
        apply_application_document_mapping(&mut onboarding.documents, &onboarding.attributes);
        if let Some(update) = payload.application_form.as_ref() {
            validate_application_update(tenant_slug, update, published_application.as_ref())?;
        }
        if published_application.is_some()
            && onboarding.stage == crate::domain::OnboardingStage::DataReview
        {
            onboarding
                .attributes
                .insert("applicationFormRequired".into(), json!(true));
        }
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
        let mut result = {
            let context = EngineContext::new(actor.user_id.clone(), Utc::now(), &services)
                .with_reason(reason)
                .with_payload(payload);
            apply_action(&definition, &onboarding, action, &context).await
        };
        if let Some(form) = published_application.as_ref()
            && let Some(application) = result
                .case
                .attributes
                .get_mut("applicationForm")
                .and_then(Value::as_object_mut)
        {
            application.insert("formType".into(), json!("application"));
            if let Some(schema) = form.get("schema") {
                application.insert("schema".into(), schema.clone());
            }
            apply_application_document_mapping(&mut result.case.documents, &result.case.attributes);
        }
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

/// Backfill an immutable schema only when an older case references the exact
/// form revision that is still published. This never applies a newer Form
/// Builder revision to an older submission.
fn hydrate_matching_application_schema(
    attributes: &mut serde_json::Map<String, Value>,
    published: Option<&Value>,
) {
    let Some(published) = published else {
        return;
    };
    let Some(application) = attributes
        .get_mut("applicationForm")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if application
        .get("schema")
        .is_some_and(|schema| !schema.is_null())
    {
        return;
    }
    let matches_form = application.get("formId").and_then(Value::as_str)
        == published.get("id").and_then(Value::as_str);
    let matches_version = application.get("formVersion").and_then(Value::as_i64)
        == published.get("version").and_then(Value::as_i64);
    if !matches_form || !matches_version {
        return;
    }
    let Some(schema) = published.get("schema") else {
        return;
    };
    application.insert("formType".into(), json!("application"));
    application.insert("schema".into(), schema.clone());
}

fn validate_application_update(
    tenant_slug: &str,
    update: &ApplicationFormUpdate,
    published: Option<&Value>,
) -> Result<(), DeskError> {
    let form = published.ok_or_else(|| {
        DeskError::Conflict("No published Admissions Application form is available".into())
    })?;
    let expected_id = form.get("id").and_then(Value::as_str).unwrap_or_default();
    let expected_version = form
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if update.form_id != expected_id || i64::from(update.form_version) != expected_version {
        return Err(DeskError::Conflict(
            "The Application form changed; refresh the desk before saving".into(),
        ));
    }
    if !matches!(update.status.as_str(), "draft" | "submitted") {
        return Err(DeskError::Conflict(
            "Application status must be draft or submitted".into(),
        ));
    }
    if update.status != "submitted" {
        return Ok(());
    }

    let sections = form
        .get("schema")
        .and_then(|schema| schema.get("sections"))
        .and_then(Value::as_array)
        .or_else(|| form.get("schema").and_then(Value::as_array));
    let Some(sections) = sections else {
        return Ok(());
    };
    let fields = sections
        .iter()
        .filter_map(|section| section.get("fields").and_then(Value::as_array))
        .flatten()
        .collect::<Vec<_>>();
    let missing = fields
        .iter()
        .filter(|field| {
            field
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|field| {
            let label = field
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("Required field");
            let key = field
                .get("key")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| application_field_key(label));
            let present = update.data.get(&key).is_some_and(application_value_present);
            (!present).then(|| label.to_owned())
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        Err(DeskError::Conflict(format!(
            "Complete required application field(s): {}",
            missing.join(", ")
        )))
    } else {
        for field in fields {
            let field_type = field
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !field_type.eq_ignore_ascii_case("upload")
                && !field_type.eq_ignore_ascii_case("image upload")
            {
                continue;
            }
            let label = field.get("label").and_then(Value::as_str).unwrap_or("File");
            let key = field
                .get("key")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| application_field_key(label));
            if let Some(value) = update.data.get(&key) {
                validate_application_media(tenant_slug, value, label)?;
            }
        }
        Ok(())
    }
}

fn validate_application_media(
    tenant_slug: &str,
    value: &Value,
    label: &str,
) -> Result<(), DeskError> {
    let object = value.as_object().ok_or_else(|| {
        DeskError::Conflict(format!("{label} must be uploaded before submission"))
    })?;
    let expected_prefix = format!("supercampus/{tenant_slug}/media/");
    let valid = object.get("storage").and_then(Value::as_str) == Some("cloudinary")
        && object
            .get("secureUrl")
            .and_then(Value::as_str)
            .is_some_and(|url| url.starts_with("https://"))
        && object
            .get("publicId")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(&expected_prefix))
        && object
            .get("fileName")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.trim().is_empty());
    if valid {
        Ok(())
    } else {
        Err(DeskError::Conflict(format!(
            "{label} does not contain a valid tenant Cloudinary upload"
        )))
    }
}

fn application_field_key(label: &str) -> String {
    label
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn application_value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(_) => true,
    }
}

fn build_snapshot(
    definition: &crate::domain::WorkflowDefinition,
    application_form: Option<Value>,
    desk_form: Option<Value>,
    cases: Vec<OnboardingCase>,
    audit: Vec<Value>,
    events: Vec<Value>,
) -> DeskSnapshot {
    let queues = summarise_queues(&cases);
    DeskSnapshot {
        definition: serde_json::to_value(definition).unwrap_or(Value::Null),
        application_form,
        desk_form,
        queues: json!(queues),
        cases,
        audit,
        events,
    }
}

#[cfg(test)]
mod application_form_validation_tests {
    use super::*;

    fn published_form() -> Value {
        json!({
            "id": "form-1",
            "version": 2,
            "schema": { "sections": [{
                "section": "Applicant",
                "fields": [
                    { "key": "full_name", "label": "Full name", "required": true },
                    { "label": "Programme", "required": true }
                ]
            }]}
        })
    }

    #[test]
    fn submitted_application_requires_every_server_defined_field() {
        let update = ApplicationFormUpdate {
            form_id: "form-1".into(),
            form_version: 2,
            status: "submitted".into(),
            data: json!({ "full_name": "Asha" }).as_object().cloned().unwrap(),
        };
        let error = validate_application_update("tenant-local", &update, Some(&published_form()))
            .unwrap_err();
        assert!(error.to_string().contains("Programme"));
    }

    #[test]
    fn submitted_application_upload_must_belong_to_the_case_tenant() {
        let form = json!({
            "id": "form-1",
            "version": 2,
            "schema": { "sections": [{ "fields": [
                { "key": "marksheet", "label": "Marksheet", "type": "Upload", "required": true }
            ]}]}
        });
        let update = ApplicationFormUpdate {
            form_id: "form-1".into(),
            form_version: 2,
            status: "submitted".into(),
            data: json!({ "marksheet": {
                "storage": "cloudinary",
                "fileName": "marksheet.pdf",
                "secureUrl": "https://res.cloudinary.com/example/image/upload/marksheet.pdf",
                "publicId": "supercampus/another-tenant/media/marksheet"
            }})
            .as_object()
            .cloned()
            .unwrap(),
        };
        let error = validate_application_update("tenant-local", &update, Some(&form)).unwrap_err();
        assert!(error.to_string().contains("tenant Cloudinary upload"));
    }

    #[test]
    fn stale_application_form_versions_are_rejected() {
        let update = ApplicationFormUpdate {
            form_id: "form-1".into(),
            form_version: 1,
            status: "draft".into(),
            data: serde_json::Map::new(),
        };
        let error = validate_application_update("tenant-local", &update, Some(&published_form()))
            .unwrap_err();
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn matching_legacy_submission_receives_its_published_schema() {
        let mut attributes = json!({
            "applicationForm": {
                "formId": "form-1",
                "formVersion": 2,
                "status": "submitted",
                "data": { "full_name": "Asha" }
            }
        })
        .as_object()
        .cloned()
        .unwrap();

        hydrate_matching_application_schema(&mut attributes, Some(&published_form()));

        let application = attributes["applicationForm"].as_object().unwrap();
        assert_eq!(application.get("formType"), Some(&json!("application")));
        assert!(application.get("schema").is_some());
    }

    #[test]
    fn newer_schema_is_not_attached_to_an_older_submission() {
        let mut attributes = json!({
            "applicationForm": {
                "formId": "form-1",
                "formVersion": 1,
                "status": "submitted",
                "data": { "full_name": "Asha" }
            }
        })
        .as_object()
        .cloned()
        .unwrap();

        hydrate_matching_application_schema(&mut attributes, Some(&published_form()));

        assert!(attributes["applicationForm"].get("schema").is_none());
    }
}
