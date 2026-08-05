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
