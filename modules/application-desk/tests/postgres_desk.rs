//! Integration tests that need a real PostgreSQL instance.
//!
//! These prove the guarantees the pure engine cannot: that sequence allocation
//! is genuinely transactional under concurrency, that a replayed transition
//! reuses its effects rather than creating a second student, and that a tenant
//! cannot see or act on another tenant's case.
//!
//! Run with:
//!   `DATABASE_URL=postgres://... cargo test -p supercampus-application-desk -- --ignored`

use std::collections::HashSet;

use supercampus_application_desk::{
    application::{ActionRequest, ActorContext, ApplicationDeskService},
    domain::{
        ActionKind, ActionPayload, AdmissionTrigger, DocumentState, IdentityMatchKind,
        OnboardingStage, OnboardingStatus, types::ApplicantSnapshot,
    },
    infrastructure::postgres::PostgresDeskRepository,
};
use supercampus_database::Database;
use uuid::Uuid;

async fn connect() -> Database {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.migrate().await.expect("database migration");
    database
}

fn tenant_slug() -> String {
    format!("desk-it-{}", Uuid::new_v4())
}

/// Case references are unique per tenant, so assertions must scope by tenant.
async fn tenant_uuid(database: &Database, slug: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug = $1")
        .bind(slug)
        .fetch_one(database.pool())
        .await
        .expect("tenant uuid")
}

async fn create_crm_lead(database: &Database, tenant: &str, suffix: &str) -> Uuid {
    let repository = PostgresDeskRepository::new(database.clone());
    let (tenant_id, mut transaction) = repository
        .begin_tenant(tenant)
        .await
        .expect("tenant transaction");
    let lead_id = sqlx::query_scalar(
        r#"INSERT INTO crm.leads
           (tenant_id, full_name, email, source, created_by)
           VALUES ($1, $2, $3, 'integration-test', 'integration-test')
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(format!("Application link {suffix}"))
    .bind(format!("application-link-{suffix}@supercampus.test"))
    .fetch_one(&mut *transaction)
    .await
    .expect("create CRM lead");
    transaction.commit().await.expect("commit CRM lead");
    lead_id
}

fn actor() -> ActorContext {
    ActorContext {
        user_id: "officer-1".into(),
        roles: vec!["registrar".into()],
        permissions: vec!["*".into()],
    }
}

fn trigger(suffix: &str) -> AdmissionTrigger {
    AdmissionTrigger {
        tenant_id: String::new(),
        applicant_id: format!("APP-{suffix}"),
        application_id: format!("APL-{suffix}"),
        admission_id: format!("ADM-{suffix}"),
        crm_lead_id: None,
        admission_status: "CONFIRMED".into(),
        academic_year: Some("2026".into()),
        admission_category: Some("GENERAL".into()),
        program_id: Some("prog-btech-cse".into()),
        department_id: Some("dept-cse".into()),
        campus_id: Some("campus-main".into()),
        batch_id: Some("batch-2026".into()),
        fee_paid: true,
        applicant: ApplicantSnapshot {
            full_name: Some("Integration Applicant".into()),
            email: Some(format!("{suffix}@supercampus.test").to_ascii_lowercase()),
            phone: None,
            guardian_name: None,
            guardian_email: None,
        },
    }
}

/// Payload that satisfies every guard in one go.
fn satisfying_payload() -> ActionPayload {
    serde_json::from_value(serde_json::json!({
        "identityMatch": "NO_MATCH",
        "documents": [
            { "type": "certificate-10", "state": "VERIFIED" },
            { "type": "certificate-12", "state": "VERIFIED" },
            { "type": "transfer-certificate", "state": "VERIFIED" },
            { "type": "identity-proof", "state": "VERIFIED" },
            { "type": "photo", "state": "VERIFIED" },
            { "type": "admission-proof", "state": "VERIFIED" },
        ],
        "finance": "CLEARED",
        "sectionId": "sec-a",
    }))
    .expect("payload")
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn concurrent_number_allocation_never_collides() {
    let database = connect().await;
    let repository = PostgresDeskRepository::new(database);
    let tenant = tenant_slug();

    // Warm the tenant row so every task races on the sequence, not on tenant creation.
    let (_, transaction) = repository.begin_tenant(&tenant).await.expect("tenant");
    transaction.commit().await.expect("commit");

    const CONCURRENCY: usize = 32;
    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let repository = repository.clone();
        let tenant = tenant.clone();
        handles.push(tokio::spawn(async move {
            let (tenant_id, mut transaction) =
                repository.begin_tenant(&tenant).await.expect("tenant");
            let value =
                PostgresDeskRepository::allocate_sequence(&mut transaction, tenant_id, "2026:CSE")
                    .await
                    .expect("allocate");
            transaction.commit().await.expect("commit");
            value
        }));
    }

    let mut issued = Vec::with_capacity(CONCURRENCY);
    for handle in handles {
        issued.push(handle.await.expect("task"));
    }

    let unique: HashSet<i64> = issued.iter().copied().collect();
    assert_eq!(
        unique.len(),
        CONCURRENCY,
        "every concurrently allocated sequence value must be unique: {issued:?}"
    );
    assert_eq!(
        unique.into_iter().max(),
        Some(CONCURRENCY as i64),
        "allocation must be gapless"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn a_replayed_transition_never_creates_a_second_student() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database.clone());
    let tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    let (created, refusal, _) = service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    assert!(created, "intake refused: {refusal:?}");

    let snapshot = service.snapshot(&tenant).await.expect("snapshot");
    let case_id = snapshot.cases.first().expect("a case").id.clone();

    // Walk to STUDENT_CREATION, supplying the facts each guard needs.
    let mut guard_rail = 0;
    loop {
        let snapshot = service.snapshot(&tenant).await.expect("snapshot");
        let current = snapshot
            .cases
            .iter()
            .find(|entry| entry.id == case_id)
            .expect("case");
        if current.stage == OnboardingStage::StudentCreation {
            break;
        }
        assert!(guard_rail < 12, "did not reach STUDENT_CREATION");
        guard_rail += 1;

        let mut payload = satisfying_payload();
        if current.stage == OnboardingStage::Approval {
            // Approve each configured step in turn.
            payload =
                serde_json::from_value(serde_json::json!({ "approvalStep": 1 })).expect("payload");
            let _ = service
                .act(
                    &tenant,
                    &actor(),
                    ActionRequest {
                        case_id: case_id.clone(),
                        action: ActionKind::Approve,
                        reason: None,
                        payload: serde_json::from_value(serde_json::json!({ "approvalStep": 2 }))
                            .expect("payload"),
                    },
                )
                .await
                .expect("approve step 2");
        }

        let outcome = service
            .act(
                &tenant,
                &actor(),
                ActionRequest {
                    case_id: case_id.clone(),
                    action: ActionKind::Advance,
                    reason: None,
                    payload,
                },
            )
            .await
            .expect("advance");
        assert!(
            outcome.ok,
            "stage {:?} refused: {:?}",
            current.stage, outcome.error
        );
    }

    // Run STUDENT_CREATION, then replay the very same transition.
    let first = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Advance,
                reason: None,
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("student creation");
    assert!(first.ok, "student creation refused: {:?}", first.error);

    let created_case = first
        .snapshot
        .cases
        .iter()
        .find(|entry| entry.id == case_id)
        .expect("case")
        .clone();
    let student_number = created_case.student_number.clone().expect("student number");
    let student_id = created_case.student_id.clone().expect("student id");

    // Force the case back to STUDENT_CREATION to simulate a retry of the same step.
    let (tenant_id, mut transaction) = PostgresDeskRepository::new(database.clone())
        .begin_tenant(&tenant)
        .await
        .expect("tenant");
    let mut replay = created_case.clone();
    replay.stage = OnboardingStage::StudentCreation;
    replay.status = OnboardingStatus::Active;
    PostgresDeskRepository::upsert_case(&mut transaction, tenant_id, &replay)
        .await
        .expect("rewind case");
    transaction.commit().await.expect("commit");

    let replayed = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Advance,
                reason: None,
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("replay");
    assert!(replayed.ok, "replay refused: {:?}", replayed.error);

    let after = replayed
        .snapshot
        .cases
        .iter()
        .find(|entry| entry.id == case_id)
        .expect("case")
        .clone();
    assert_eq!(
        after.student_number.as_deref(),
        Some(student_number.as_str()),
        "the student number must be stable across a replay"
    );
    assert_eq!(after.student_id.as_deref(), Some(student_id.as_str()));

    // The decisive check: exactly one Student Master row exists for this admission.
    let students: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.students WHERE tenant_id = $1 AND admission_id = $2",
    )
    .bind(tenant_id)
    .bind(format!("ADM-{suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("count students");
    assert_eq!(students, 1, "a retry must never create a second student");

    // And the effect ledger recorded each effect exactly once.
    let effects: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM application_desk.onboarding_effect
           WHERE tenant_id = $1 AND case_id = $2 AND effect = 'create_student'"#,
    )
    .bind(tenant_id)
    .bind(&case_id)
    .fetch_one(database.pool())
    .await
    .expect("count effects");
    assert_eq!(effects, 1);
}

/// Case references restart per tenant, so two institutions both hold a case
/// called `ONB-2026-000001`. Each must record its own effect ledger; one
/// tenant's row must never suppress another's.
#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn two_tenants_sharing_a_case_reference_each_record_their_own_effects() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database.clone());

    let mut references = Vec::new();
    for _ in 0..2 {
        let tenant = tenant_slug();
        let suffix = Uuid::new_v4().to_string();
        service
            .open_case(&tenant, &actor(), trigger(&suffix))
            .await
            .expect("open case");

        let snapshot = service.snapshot(&tenant).await.expect("snapshot");
        let case_id = snapshot.cases.first().expect("a case").id.clone();
        let tenant_id = tenant_uuid(&database, &tenant).await;

        // Record an effect directly: this is the ledger write every effect makes.
        sqlx::query(
            r#"INSERT INTO application_desk.onboarding_effect
               (tenant_id, case_id, effect, result)
               VALUES ($1, $2, 'generate_number', '2026CSE001')
               ON CONFLICT ON CONSTRAINT onboarding_effect_case_effect_key DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(&case_id)
        .execute(database.pool())
        .await
        .expect("record effect");

        references.push((tenant_id, case_id));
    }

    let (first_id, first_case) = &references[0];
    let (second_id, second_case) = &references[1];
    assert_eq!(
        first_case, second_case,
        "both tenants should have produced the same case reference"
    );
    assert_ne!(first_id, second_id);

    for (tenant_id, case_id) in &references {
        let recorded: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM application_desk.onboarding_effect
               WHERE tenant_id = $1 AND case_id = $2 AND effect = 'generate_number'"#,
        )
        .bind(tenant_id)
        .bind(case_id)
        .fetch_one(database.pool())
        .await
        .expect("count effects");
        assert_eq!(
            recorded, 1,
            "each tenant must record its own effect for {case_id}"
        );
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn a_case_is_invisible_and_unactionable_from_another_tenant() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database);
    let owner = tenant_slug();
    let intruder = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    let (created, refusal, _) = service
        .open_case(&owner, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    assert!(created, "intake refused: {refusal:?}");

    let owned = service.snapshot(&owner).await.expect("snapshot");
    let case_id = owned.cases.first().expect("a case").id.clone();

    let other = service.snapshot(&intruder).await.expect("snapshot");
    assert!(
        other.cases.is_empty(),
        "another tenant must not see this case"
    );

    let result = service
        .act(
            &intruder,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Advance,
                reason: None,
                payload: ActionPayload::default(),
            },
        )
        .await;
    assert!(
        result.is_err(),
        "another tenant must not be able to act on this case"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn a_closed_case_does_not_block_re_admission() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database);
    let tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    let (created, _, _) = service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    assert!(created);

    // A second intake for the same applicant is refused while the case is live.
    let (blocked, reason, _) = service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("second intake");
    assert!(!blocked);
    assert!(
        reason
            .unwrap_or_default()
            .contains("already has onboarding"),
        "the refusal must name the existing case"
    );

    // Withdraw it, and the applicant becomes eligible again.
    let snapshot = service.snapshot(&tenant).await.expect("snapshot");
    let case_id = snapshot.cases.first().expect("a case").id.clone();
    let withdrawn = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Withdraw,
                reason: Some("applicant withdrew".into()),
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("withdraw");
    assert!(withdrawn.ok);

    let (reopened, reason, _) = service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("re-admission");
    assert!(
        reopened,
        "a withdrawn case must not block re-admission: {reason:?}"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn lead_application_relationship_is_tenant_safe_stable_and_authoritative() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database.clone());
    let tenant = tenant_slug();
    let other_tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();
    let lead_id = create_crm_lead(&database, &tenant, &suffix).await;
    let mut linked_trigger = trigger(&suffix);
    linked_trigger.crm_lead_id = Some(lead_id);

    let (created, refusal, snapshot) = service
        .open_case(&tenant, &actor(), linked_trigger.clone())
        .await
        .expect("open linked application");
    assert!(created, "linked intake refused: {refusal:?}");
    let case_id = snapshot.cases.first().expect("linked case").id.clone();

    let (duplicate, _, _) = service
        .open_case(&tenant, &actor(), linked_trigger.clone())
        .await
        .expect("idempotent duplicate intake");
    assert!(!duplicate, "a live application must not be duplicated");

    let cross_tenant = service
        .open_case(&other_tenant, &actor(), linked_trigger.clone())
        .await;
    assert!(
        cross_tenant.is_err(),
        "another tenant must not be able to connect the lead"
    );
    assert!(
        service
            .snapshot(&other_tenant)
            .await
            .expect("other tenant snapshot")
            .cases
            .is_empty(),
        "the rejected cross-tenant transaction must not leave a case behind"
    );

    let repository = PostgresDeskRepository::new(database.clone());
    let (tenant_id, mut transaction) = repository
        .begin_tenant(&tenant)
        .await
        .expect("tenant transaction");
    let relationship_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.lead_application_links WHERE tenant_id = $1 AND lead_id = $2",
    )
    .bind(tenant_id)
    .bind(lead_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("relationship count");
    assert_eq!(relationship_count, 1);
    let copied_status_columns: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM information_schema.columns
           WHERE table_schema = 'crm' AND table_name = 'lead_application_links'
             AND column_name IN ('application_id', 'admission_id', 'application_status')"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .expect("relationship columns");
    assert_eq!(
        copied_status_columns, 0,
        "CRM must store only the relationship, not an application copy"
    );
    let linked_status: String = sqlx::query_scalar(
        r#"SELECT desk.status
           FROM crm.lead_application_links link
           JOIN application_desk.cases desk
             ON desk.tenant_id = link.tenant_id AND desk.id = link.case_id
           WHERE link.tenant_id = $1 AND link.lead_id = $2"#,
    )
    .bind(tenant_id)
    .bind(lead_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("authoritative application status");
    assert_eq!(linked_status, "ACTIVE");
    let sync_history: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM crm.stage_history
           WHERE tenant_id = $1 AND lead_id = $2 AND reason = 'application_status_sync'"#,
    )
    .bind(tenant_id)
    .bind(lead_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("CRM conversion history");
    assert_eq!(sync_history, 1, "the initial conversion must be traceable");
    transaction.commit().await.expect("commit assertions");

    let withdrawn = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id,
                action: ActionKind::Withdraw,
                reason: Some("applicant withdrew".into()),
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("withdraw application");
    assert!(withdrawn.ok);

    let (reopened, refusal, _) = service
        .open_case(&tenant, &actor(), linked_trigger)
        .await
        .expect("re-admission");
    assert!(
        reopened,
        "a closed application must not block a valid re-admission: {refusal:?}"
    );

    let (_, mut transaction) = repository
        .begin_tenant(&tenant)
        .await
        .expect("tenant transaction");
    let relationships_after_readmission: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.lead_application_links WHERE tenant_id = $1 AND lead_id = $2",
    )
    .bind(tenant_id)
    .bind(lead_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("relationship history");
    assert_eq!(
        relationships_after_readmission, 2,
        "each Application Desk case keeps its own immutable conversion link"
    );
    transaction.commit().await.expect("commit assertions");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn every_transition_writes_an_audit_row() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database.clone());
    let tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    let snapshot = service.snapshot(&tenant).await.expect("snapshot");
    let case_id = snapshot.cases.first().expect("a case").id.clone();

    let held = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Hold,
                reason: Some("documents pending".into()),
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("hold");
    assert!(held.ok);

    // Case references are unique per tenant, not globally, so scope the check.
    let tenant_id = tenant_uuid(&database, &tenant).await;
    let rows: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM application_desk.audit_log
           WHERE tenant_id = $1 AND case_id = $2 AND action = 'ON_HOLD'"#,
    )
    .bind(tenant_id)
    .bind(&case_id)
    .fetch_one(database.pool())
    .await
    .expect("count audit");
    assert_eq!(rows, 1, "the hold must be recorded in the audit log");

    // The audit log is append-only: an update must be rejected outright.
    let update = sqlx::query(
        "UPDATE application_desk.audit_log SET actor = 'tamper' WHERE tenant_id = $1 AND case_id = $2",
    )
    .bind(tenant_id)
    .bind(&case_id)
    .execute(database.pool())
    .await;
    assert!(update.is_err(), "the audit log must reject updates");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn a_held_case_resumes_at_the_stage_it_was_held_on() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database);
    let tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    let snapshot = service.snapshot(&tenant).await.expect("snapshot");
    let case_id = snapshot.cases.first().expect("a case").id.clone();

    // Move off the opening stage first so resume has somewhere distinct to go.
    let advanced = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Advance,
                reason: None,
                payload: satisfying_payload(),
            },
        )
        .await
        .expect("advance");
    assert!(advanced.ok, "advance refused: {:?}", advanced.error);
    let parked = advanced
        .snapshot
        .cases
        .iter()
        .find(|entry| entry.id == case_id)
        .expect("case")
        .stage;

    service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Hold,
                reason: Some("waiting on registrar".into()),
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("hold");

    let resumed = service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Resume,
                reason: None,
                payload: ActionPayload::default(),
            },
        )
        .await
        .expect("resume");
    assert!(resumed.ok);

    let after = resumed
        .snapshot
        .cases
        .iter()
        .find(|entry| entry.id == case_id)
        .expect("case");
    assert_eq!(after.status, OnboardingStatus::Active);
    assert_eq!(
        after.stage, parked,
        "resume must return to the stage the case was held on"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn identity_and_document_facts_survive_a_reload() {
    let database = connect().await;
    let service = ApplicationDeskService::new(database);
    let tenant = tenant_slug();
    let suffix = Uuid::new_v4().to_string();

    service
        .open_case(&tenant, &actor(), trigger(&suffix))
        .await
        .expect("open case");
    let snapshot = service.snapshot(&tenant).await.expect("snapshot");
    let case_id = snapshot.cases.first().expect("a case").id.clone();

    service
        .act(
            &tenant,
            &actor(),
            ActionRequest {
                case_id: case_id.clone(),
                action: ActionKind::Verify,
                reason: None,
                payload: satisfying_payload(),
            },
        )
        .await
        .expect("verify");

    // Re-read from storage, not from the returned snapshot.
    let reloaded = service.snapshot(&tenant).await.expect("snapshot");
    let case = reloaded
        .cases
        .iter()
        .find(|entry| entry.id == case_id)
        .expect("case");

    assert_eq!(case.identity_match, Some(IdentityMatchKind::NoMatch));
    assert_eq!(case.academic.section_id.as_deref(), Some("sec-a"));
    assert!(
        case.documents
            .iter()
            .any(|document| document.state == DocumentState::Verified),
        "verified documents must persist"
    );
}
