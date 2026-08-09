//! Database-backed CRM invariants. Run with `DATABASE_URL=... cargo test
//! -p supercampus-crm --test postgres_claim -- --ignored`.

use chrono::{Duration, Utc};
use serde_json::json;
use supercampus_crm::{
    domain::CrmError,
    infrastructure::postgres::{NewLead, PostgresCrmRepository},
};
use supercampus_database::Database;
use uuid::Uuid;

async fn connect() -> Database {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.migrate().await.expect("database migration");
    database
}

fn new_lead(suffix: &str) -> NewLead {
    NewLead {
        source: "integration-test".into(),
        source_detail: json!({ "test": true }),
        full_name: format!("Claim test {suffix}"),
        email: Some(format!("claim-{suffix}@example.test")),
        phone: None,
        whatsapp: None,
        parent_name: None,
        parent_phone: None,
        academic: json!({}),
        interest: json!({}),
        priority: "medium".into(),
        follow_up_at: None,
        preferred_channel: None,
        consent_given: true,
        custom_fields: json!({}),
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary tenant rows"]
async fn concurrent_first_move_has_exactly_one_owner_and_is_tenant_isolated() {
    let database = connect().await;
    let repository = PostgresCrmRepository::new(database.clone());
    let suffix = Uuid::new_v4().to_string();
    let tenant_a = format!("crm-claim-a-{suffix}");
    let tenant_b = format!("crm-claim-b-{suffix}");
    let lead = repository
        .create_lead(&tenant_a, "creator", "test", new_lead(&suffix))
        .await
        .expect("create lead");
    assert_eq!(lead.created_by, "creator");
    assert_eq!(lead.assigned_to, None, "creation must not assign ownership");

    assert!(matches!(
        repository.find_lead(&tenant_b, lead.id).await,
        Err(CrmError::NotFound)
    ));

    let first_repository = repository.clone();
    let second_repository = repository.clone();
    let first_tenant = tenant_a.clone();
    let second_tenant = tenant_a.clone();
    let (first, second) = tokio::join!(
        first_repository.transition(
            &first_tenant,
            lead.id,
            "contact_attempted",
            "contacted",
            "counsellor-a",
            "counsellor",
            Some("race-a"),
            None,
            None,
            None,
            true,
            false,
        ),
        second_repository.transition(
            &second_tenant,
            lead.id,
            "contact_attempted",
            "contacted",
            "counsellor-b",
            "counsellor",
            Some("race-b"),
            None,
            None,
            None,
            true,
            false,
        ),
    );
    let winners = usize::from(first.is_ok()) + usize::from(second.is_ok());
    let conflicts = usize::from(matches!(&first, Err(CrmError::Conflict(_))))
        + usize::from(matches!(&second, Err(CrmError::Conflict(_))));
    assert_eq!(winners, 1);
    assert_eq!(conflicts, 1);
    let moved = repository
        .find_lead(&tenant_a, lead.id)
        .await
        .expect("moved lead");
    let expected_owner = if first.is_ok() {
        "counsellor-a"
    } else {
        "counsellor-b"
    };
    assert_eq!(moved.assigned_to.as_deref(), Some(expected_owner));
    assert_ne!(
        moved.assigned_to.as_deref(),
        Some(moved.created_by.as_str())
    );

    sqlx::query("DELETE FROM platform.tenants WHERE slug = ANY($1)")
        .bind(vec![tenant_a, tenant_b])
        .execute(database.pool())
        .await
        .expect("cleanup test tenants");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary tenant rows"]
async fn first_move_claims_and_owner_approval_is_single_use() {
    let database = connect().await;
    let repository = PostgresCrmRepository::new(database.clone());
    let suffix = Uuid::new_v4().to_string();
    let tenant = format!("crm-owner-move-{suffix}");
    let lead = repository
        .create_lead(&tenant, "creator", "test", new_lead(&suffix))
        .await
        .expect("create lead");
    assert_eq!(lead.created_by, "creator");
    assert_eq!(
        lead.assigned_to, None,
        "creator and owner are separate fields"
    );

    let claimed = repository
        .transition(
            &tenant,
            lead.id,
            "contact_attempted",
            "contacted",
            "owner-a",
            "counsellor",
            Some("first movement"),
            None,
            None,
            None,
            true,
            false,
        )
        .await
        .expect("first move claims lead");
    assert_eq!(claimed.assigned_to.as_deref(), Some("owner-a"));

    let direct_non_owner = repository
        .transition(
            &tenant,
            lead.id,
            "contacted",
            "nurture",
            "user-b",
            "counsellor",
            Some("direct attempt"),
            None,
            None,
            None,
            true,
            false,
        )
        .await;
    assert!(matches!(direct_non_owner, Err(CrmError::Conflict(_))));

    let request = repository
        .create_move_request(
            &tenant,
            lead.id,
            "user-b",
            "contacted",
            "nurture",
            Some("please move"),
            None,
        )
        .await
        .expect("create request");
    let approved = repository
        .decide_move_request(
            &tenant,
            request.id,
            "owner-a",
            true,
            Some("approved"),
            "counsellor",
        )
        .await
        .expect("approve request");
    assert_eq!(approved.status, "approved");
    let moved = repository
        .find_lead(&tenant, lead.id)
        .await
        .expect("moved lead");
    assert_eq!(moved.stage_key, "contacted");
    assert_eq!(moved.assigned_to.as_deref(), Some("owner-a"));
    assert!(matches!(
        repository
            .decide_move_request(&tenant, request.id, "owner-a", true, None, "counsellor")
            .await,
        Err(CrmError::Conflict(_))
    ));

    repository
        .send_communication(
            &tenant,
            lead.id,
            "note",
            None,
            Some("Lead note"),
            json!({ "text": "Persistent card-workspace note" }),
            None,
            "owner-a",
        )
        .await
        .expect("persist note");
    repository
        .create_lead_task(
            &tenant,
            lead.id,
            "owner-a",
            "Persistent follow-up task",
            Utc::now() + Duration::days(1),
            "high",
        )
        .await
        .expect("persist task");
    let timeline = repository
        .timeline(&tenant, lead.id)
        .await
        .expect("timeline");
    assert_eq!(timeline["communications"].as_array().map(Vec::len), Some(1));
    assert_eq!(timeline["tasks"].as_array().map(Vec::len), Some(1));

    sqlx::query("DELETE FROM platform.tenants WHERE slug = $1")
        .bind(&tenant)
        .execute(database.pool())
        .await
        .expect("cleanup test tenant");
}
