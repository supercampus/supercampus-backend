use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use supercampus_crm::api::routes;
use tower::ServiceExt;

#[tokio::test]
async fn crm_health_is_available_without_database_or_identity() {
    let response = routes::router(None)
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn protected_crm_routes_require_trusted_identity_context() {
    for uri in ["/leads", "/dashboard/operations", "/campaigns"] {
        let response = routes::router(None)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn claim_contract_rejects_a_target_counsellor() {
    let response = routes::router(None)
        .oneshot(
            Request::post("/leads/11111111-1111-4111-8111-111111111111/claim")
                .header("content-type", "application/json")
                .header("x-tenant-id", "tenant-a")
                .header("x-user-id", "counsellor-a")
                .header("x-user-roles", "[\"counsellor\"]")
                .header("x-user-permissions", "[\"crm.leads.claim\"]")
                .header("x-permission-scopes", "{}")
                .body(Body::from(r#"{"userId":"counsellor-b"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
