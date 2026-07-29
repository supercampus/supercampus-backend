use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use supercampus_platform_api::{app, state::AppState};
use tower::ServiceExt;

#[tokio::test]
async fn health_and_module_catalog_are_available() {
    let app = app(AppState::default());
    let health = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let modules = app
        .oneshot(Request::get("/api/v1/modules").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(modules.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(modules.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 12);
}

#[tokio::test]
async fn module_records_are_tenant_scoped_and_mutable() {
    let app = app(AppState::default());
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v1/admissions/records")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-tenant-id", "tenant-a")
                .body(Body::from(
                    r#"{"recordType":"application","data":{"applicant":"Ada"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_body: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), usize::MAX).await.unwrap()).unwrap();
    let id = created_body["data"]["id"].as_str().unwrap();

    let tenant_a = app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tenant_a_body: Value =
        serde_json::from_slice(&to_bytes(tenant_a.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(tenant_a_body["data"].as_array().unwrap().len(), 1);

    let tenant_b = app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header("x-tenant-id", "tenant-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tenant_b_body: Value =
        serde_json::from_slice(&to_bytes(tenant_b.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(tenant_b_body["data"].as_array().unwrap().is_empty());

    let deleted = app
        .oneshot(
            Request::delete(format!("/api/v1/admissions/records/{id}"))
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn local_login_cookie_unlocks_frontend_state_endpoint() {
    let app = app(AppState::default());
    let login = app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"email":"student@supercampus.local","password":"SuperCampus@123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let state = app
        .oneshot(
            Request::get("/api/state")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(state.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["version"], 0);
    assert_eq!(body["data"]["state"]["persona"], "hosteller");
}
