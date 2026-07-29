use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use supercampus_database::Database;
use supercampus_platform_api::{app, state::AppState};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn postgres_state_survives_app_state_recreation() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.migrate().await.expect("database migration");

    let tenant = format!("integration-{}", Uuid::new_v4());
    let first_app = app(AppState::with_database(database.clone()));
    let created = first_app
        .clone()
        .oneshot(
            Request::post("/api/v1/admissions/records")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-tenant-id", &tenant)
                .body(Body::from(
                    r#"{"recordType":"application","data":{"applicant":"PostgreSQL"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let configured = first_app
        .clone()
        .oneshot(
            Request::put("/api/v1/configuration/navigation")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-tenant-id", &tenant)
                .body(Body::from(r#"{"value":{"landingPage":"/admin"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(configured.status(), StatusCode::OK);

    let login = first_app
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

    let recreated_app = app(AppState::with_database(database.clone()));
    let records = recreated_app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header("x-tenant-id", &tenant)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let records_body: Value =
        serde_json::from_slice(&to_bytes(records.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(records_body["data"].as_array().unwrap().len(), 1);

    let configuration = recreated_app
        .clone()
        .oneshot(
            Request::get("/api/v1/configuration/navigation")
                .header("x-tenant-id", &tenant)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(configuration.status(), StatusCode::OK);

    let me = recreated_app
        .oneshot(
            Request::get("/api/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    sqlx::query("DELETE FROM platform.tenants WHERE slug = $1")
        .bind(&tenant)
        .execute(database.pool())
        .await
        .expect("clean temporary tenant");
}
