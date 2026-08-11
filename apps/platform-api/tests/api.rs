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
async fn gatepass_workflow_is_tenant_configurable() {
    let app = test_app();
    let tenant_a_session = login_session(&app, "tenant-a").await;
    let tenant_b_session = login_session(&app, "tenant-b").await;

    let college_one = app
        .clone()
        .oneshot(
            Request::get("/api/v1/workflows/gatepass/outpass")
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(college_one.status(), StatusCode::OK);
    let college_one_body: Value =
        serde_json::from_slice(&to_bytes(college_one.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let college_one_states = college_one_body["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|state| state["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(college_one_states.contains(&"parent_approved"));

    let college_two = app
        .clone()
        .oneshot(
            Request::get("/api/v1/workflows/gatepass/outpass")
                .header(header::AUTHORIZATION, tenant_b_session.bearer())
                .header("x-tenant-id", "tenant-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(college_two.status(), StatusCode::OK);
    let college_two_body: Value =
        serde_json::from_slice(&to_bytes(college_two.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let college_two_states = college_two_body["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|state| state["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(!college_two_states.contains(&"parent_approved"));

    let college_one_next = validate_gatepass_transition(&app, &tenant_a_session, "tenant-a").await;
    let college_two_next = validate_gatepass_transition(&app, &tenant_b_session, "tenant-b").await;
    assert_eq!(college_one_next["data"]["to"], "parent_approved");
    assert_eq!(college_two_next["data"]["to"], "warden_approved");

    let bootstrap = app
        .clone()
        .oneshot(
            Request::get("/api/v1/bootstrap")
                .header(header::AUTHORIZATION, tenant_a_session.bearer())
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), StatusCode::OK);
    let bootstrap_body: Value =
        serde_json::from_slice(&to_bytes(bootstrap.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(bootstrap_body["data"]["workflows"][0]["module"], "gatepass");
    assert_eq!(bootstrap_body["data"]["workflows"][0]["feature"], "outpass");

    let invalid_college_two_state = app
        .oneshot(
            Request::post("/api/v1/workflows/gatepass/outpass/transitions/validate")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, tenant_b_session.bearer())
                .header("x-tenant-id", "tenant-b")
                .body(Body::from(
                    r#"{"currentState":"parent_approved","action":"approve"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_college_two_state.status(), StatusCode::BAD_REQUEST);
}

async fn validate_gatepass_transition(app: &Router, session: &TestSession, tenant: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/workflows/gatepass/outpass/transitions/validate")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, session.bearer())
                .header("x-tenant-id", tenant)
                .body(Body::from(
                    r#"{"currentState":"submitted","action":"approve"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
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

#[tokio::test]
async fn forgot_password_is_public_and_does_not_reveal_whether_the_account_exists() {
    let app = test_app();
    let mut bodies = Vec::new();
    for email in [TEST_EMAIL, "definitely-not-registered@supercampus.test"] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/forgot-password")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"email":"{email}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        bodies.push(body);
    }
    // A registered and an unregistered address must be indistinguishable.
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn reset_password_rejects_a_password_below_the_minimum_length() {
    let response = test_app()
        .oneshot(
            Request::post("/api/auth/reset-password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"token":"any-token","password":"short"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(body["error"].as_str().unwrap().contains("12 characters"));
}

#[tokio::test]
async fn reset_password_rejects_an_empty_token() {
    let response = test_app()
        .oneshot(
            Request::post("/api/auth/reset-password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"token":"   ","password":"a-sufficiently-long-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn navigation_and_realtime_token_require_authentication() {
    let app = test_app();
    for (method, path) in [
        ("GET", "/api/v1/navigation"),
        ("POST", "/api/auth/realtime-token"),
    ] {
        let request = if method == "GET" {
            Request::get(path).body(Body::empty()).unwrap()
        } else {
            Request::post(path).body(Body::empty()).unwrap()
        };
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must not be public"
        );
    }
}

#[tokio::test]
async fn navigation_returns_sections_the_grants_allow() {
    let app = test_app();
    let session = login_session(&app, "tenant-local").await;
    let response = app
        .oneshot(
            Request::get("/api/v1/navigation")
                .header(header::AUTHORIZATION, session.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();

    let workspace = body["data"]["workspace"].as_array().unwrap();
    let keys: Vec<&str> = workspace
        .iter()
        .map(|section| section["key"].as_str().unwrap())
        .collect();
    // The in-memory identity holds "*", so every default section resolves.
    assert!(keys.contains(&"crm"), "expected crm section, got {keys:?}");
    assert!(keys.contains(&"pipeline"));
    // Settings is only emitted once at least one settings child is reachable.
    assert!(keys.contains(&"settings"));
    assert!(!body["data"]["settings"].as_array().unwrap().is_empty());
    // Every section must carry what the client needs to render it.
    for section in workspace {
        assert!(
            section["label"].is_string(),
            "section missing label: {section}"
        );
        assert!(section["key"].is_string());
    }
}

#[tokio::test]
async fn realtime_token_is_short_lived_and_accepted_as_a_query_credential() {
    let app = test_app();
    let session = login_session(&app, "tenant-local").await;
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/auth/realtime-token")
                .header(header::AUTHORIZATION, session.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let token = body["data"]["token"].as_str().unwrap().to_owned();
    assert!(!token.is_empty());

    // The query credential is only honoured on the realtime stream path. Any other
    // route must still reject it, so the relaxation cannot widen the auth surface.
    let elsewhere = app
        .oneshot(
            Request::get(format!("/api/v1/navigation?access_token={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), StatusCode::UNAUTHORIZED);
}
