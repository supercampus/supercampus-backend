use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use supercampus_platform_api::{app, state::AppState};
use tower::ServiceExt;

struct TestSession {
    access_token: String,
    access_cookie: String,
    refresh_cookie: String,
}

impl TestSession {
    fn bearer(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

const TEST_EMAIL: &str = "student@supercampus.test";
const TENANT_A_EMAIL: &str = "student-a@supercampus.test";
const TENANT_B_EMAIL: &str = "student-b@supercampus.test";
const TEST_PASSWORD: &str = "integration-test-password";

fn test_app() -> Router {
    app(AppState::default()
        .with_memory_identity(
            TEST_EMAIL,
            TEST_PASSWORD,
            "tenant-local",
            vec!["admissions_manager".into()],
        )
        .with_memory_identity(
            TENANT_A_EMAIL,
            TEST_PASSWORD,
            "tenant-a",
            vec!["admissions_manager".into()],
        )
        .with_memory_identity(
            TENANT_B_EMAIL,
            TEST_PASSWORD,
            "tenant-b",
            vec!["admissions_manager".into()],
        ))
}

async fn login_session(app: &Router, tenant_id: &str) -> TestSession {
    let email = match tenant_id {
        "tenant-a" => TENANT_A_EMAIL,
        "tenant-b" => TENANT_B_EMAIL,
        _ => TEST_EMAIL,
    };
    let login = app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{email}","password":"{TEST_PASSWORD}"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookies = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let access_cookie = cookies
        .iter()
        .find(|cookie| cookie.starts_with("sc_access="))
        .unwrap()
        .clone();
    let refresh_cookie = cookies
        .iter()
        .find(|cookie| cookie.starts_with("sc_session="))
        .unwrap()
        .clone();
    let body: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), usize::MAX).await.unwrap()).unwrap();
    TestSession {
        access_token: body["data"]["accessToken"].as_str().unwrap().to_owned(),
        access_cookie,
        refresh_cookie,
    }
}

#[tokio::test]
async fn login_rejects_client_selected_tenant() {
    let response = test_app()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{TEST_EMAIL}","password":"{TEST_PASSWORD}","tenantId":"tenant-b"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
#[tokio::test]
async fn health_and_module_catalog_are_available() {
    let app = test_app();
    let health = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let crm_health = app
        .clone()
        .oneshot(
            Request::get("/api/v1/crm/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(crm_health.status(), StatusCode::OK);

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/modules").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let session = login_session(&app, "tenant-local").await;
    let modules = app
        .oneshot(
            Request::get("/api/v1/modules")
                .header(header::AUTHORIZATION, session.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(modules.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(modules.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 12);
}

#[tokio::test]
async fn module_records_are_tenant_scoped_and_mutable() {
    let app = test_app();
    let tenant_a_session = login_session(&app, "tenant-a").await;
    let tenant_b_session = login_session(&app, "tenant-b").await;
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v1/admissions/records")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
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
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tenant_a_body: Value =
        serde_json::from_slice(&to_bytes(tenant_a.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(tenant_a_body["data"].as_array().unwrap().len(), 1);

    let spoofed_tenant = app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
                .header("x-tenant-id", "tenant-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(spoofed_tenant.status(), StatusCode::FORBIDDEN);

    let tenant_b = app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header(header::AUTHORIZATION, tenant_b_session.bearer())
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
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
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
    let app = test_app();
    let session = login_session(&app, "tenant-local").await;
    let state = app
        .clone()
        .oneshot(
            Request::get("/api/state")
                .header(header::COOKIE, session.access_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(state.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["version"], 0);
    assert_eq!(body["data"]["state"]["persona"], "dayscholar");
}

#[tokio::test]
async fn refresh_rotation_and_logout_revoke_the_server_session() {
    let app = test_app();
    let session = login_session(&app, "tenant-local").await;
    let refreshed = app
        .clone()
        .oneshot(
            Request::post("/api/auth/refresh")
                .header(header::COOKIE, &session.refresh_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed_cookies = refreshed
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let new_refresh_cookie = refreshed_cookies
        .iter()
        .find(|cookie| cookie.starts_with("sc_session="))
        .unwrap()
        .clone();
    assert_ne!(new_refresh_cookie, session.refresh_cookie);
    let refreshed_body: Value =
        serde_json::from_slice(&to_bytes(refreshed.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let new_access_token = refreshed_body["data"]["accessToken"]
        .as_str()
        .unwrap()
        .to_owned();

    let reused = app
        .clone()
        .oneshot(
            Request::post("/api/auth/refresh")
                .header(header::COOKIE, session.refresh_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);

    let revoked_access = app
        .clone()
        .oneshot(
            Request::get("/api/auth/me")
                .header(header::AUTHORIZATION, format!("Bearer {new_access_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_access.status(), StatusCode::UNAUTHORIZED);

    let logout = app
        .oneshot(
            Request::post("/api/auth/logout")
                .header(header::COOKIE, new_refresh_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn jwt_claims_override_spoofed_crm_identity_headers() {
    let app = test_app();
    let login = app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{TEST_EMAIL}","password":"{TEST_PASSWORD}"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), usize::MAX).await.unwrap()).unwrap();
    let token = body["data"]["accessToken"].as_str().unwrap();

    let permissions = app
        .oneshot(
            Request::get("/api/v1/crm/permissions/effective")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("x-user-id", "spoofed-user")
                .header("x-user-role", "prospective_student")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(permissions.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(permissions.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_ne!(body["data"]["userId"], "spoofed-user");
    assert_eq!(body["data"]["primaryRole"], "admissions_manager");
    assert_eq!(body["data"]["permissions"][0], "*");
}
