use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
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
    let email = format!("integration-{}@supercampus.test", Uuid::new_v4());
    let password = "integration-test-password";
    let tenant_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO platform.tenants (slug, code, name)
           VALUES ($1, $2, $3) RETURNING id"#,
    )
    .bind(&tenant)
    .bind(tenant.to_uppercase())
    .bind("Integration Test Campus")
    .fetch_one(database.pool())
    .await
    .expect("create integration tenant");
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO identity.users
           (email, password_hash, display_name, initials, account_type)
           VALUES ($1, crypt($2, gen_salt('bf', 4)), 'Integration User', 'IU', 'staff')
           RETURNING id"#,
    )
    .bind(&email)
    .bind(password)
    .fetch_one(database.pool())
    .await
    .expect("create integration identity");
    sqlx::query(
        r#"INSERT INTO identity.tenant_memberships
           (tenant_id, user_id, roles, is_primary, profile)
           VALUES ($1, $2, $3, true, '{}'::jsonb)"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(vec!["admissions_manager"])
    .execute(database.pool())
    .await
    .expect("create integration membership");
    let first_app = app(AppState::with_database(database.clone()));
    let login = first_app
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{email}","password":"{password}"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login_body: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), usize::MAX).await.unwrap()).unwrap();
    let access_token = login_body["data"]["accessToken"]
        .as_str()
        .unwrap()
        .to_owned();
    let authorization = format!("Bearer {access_token}");

    let created = first_app
        .clone()
        .oneshot(
            Request::post("/api/v1/admissions/records")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, &authorization)
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
                .header(header::AUTHORIZATION, &authorization)
                .header("x-tenant-id", &tenant)
                .body(Body::from(r#"{"value":{"landingPage":"/admin"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(configured.status(), StatusCode::OK);

    let recreated_app = app(AppState::with_database(database.clone()));
    let records = recreated_app
        .clone()
        .oneshot(
            Request::get("/api/v1/admissions/records")
                .header(header::AUTHORIZATION, &authorization)
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
                .header(header::AUTHORIZATION, &authorization)
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
                .header(header::AUTHORIZATION, authorization)
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

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn role_permission_changes_apply_on_the_next_request_without_a_new_token() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.migrate().await.expect("database migration");

    let suffix = Uuid::new_v4();
    let tenant = format!("rbac-{suffix}");
    let admin_email = format!("rbac-admin-{suffix}@supercampus.test");
    let reader_email = format!("rbac-reader-{suffix}@supercampus.test");
    let admin_password = "rbac-admin-test-password";
    let reader_password = "rbac-reader-test-password";
    let tenant_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO platform.tenants (slug, code, name)
           VALUES ($1, $2, 'RBAC Integration Campus') RETURNING id"#,
    )
    .bind(&tenant)
    .bind(format!("RBAC_{suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("create RBAC tenant");
    let admin_user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO identity.users
           (email, password_hash, display_name, initials, account_type)
           VALUES ($1, crypt($2, gen_salt('bf', 4)), 'RBAC Admin', 'RA', 'staff')
           RETURNING id"#,
    )
    .bind(&admin_email)
    .bind(admin_password)
    .fetch_one(database.pool())
    .await
    .expect("create RBAC admin");
    let tenant_admin_role_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM authz.roles WHERE tenant_id = $1 AND role_key = 'tenant_admin'",
    )
    .bind(tenant_id)
    .fetch_one(database.pool())
    .await
    .expect("tenant admin bootstrap role");
    sqlx::query(
        r#"INSERT INTO identity.tenant_memberships
           (tenant_id, user_id, roles, is_primary, profile)
           VALUES ($1, $2, ARRAY['tenant_admin'], true, '{}'::jsonb)"#,
    )
    .bind(tenant_id)
    .bind(admin_user_id)
    .execute(database.pool())
    .await
    .expect("create admin membership");
    sqlx::query(
        r#"INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
           VALUES ($1, $2, $3, 'integration-test')"#,
    )
    .bind(tenant_id)
    .bind(admin_user_id)
    .bind(tenant_admin_role_id)
    .execute(database.pool())
    .await
    .expect("assign tenant admin role");

    let application = app(AppState::with_database(database.clone()));
    let admin_login = application
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{admin_email}","password":"{admin_password}"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(admin_login.status(), StatusCode::OK);
    let admin_body: Value =
        serde_json::from_slice(&to_bytes(admin_login.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let admin_authorization = format!(
        "Bearer {}",
        admin_body["data"]["accessToken"].as_str().unwrap()
    );

    let permission_catalog = application
        .clone()
        .oneshot(
            Request::get("/api/v1/authorization/permissions")
                .header(header::AUTHORIZATION, &admin_authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let permission_status = permission_catalog.status();
    let permission_bytes = to_bytes(permission_catalog.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        permission_status,
        StatusCode::OK,
        "permission catalog response: {}",
        String::from_utf8_lossy(&permission_bytes)
    );
    let permission_body: Value = serde_json::from_slice(&permission_bytes).unwrap();
    let permissions = permission_body["data"].as_array().unwrap();
    let campaigns_create = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.campaigns.create")
        .unwrap();
    let campaigns_update = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.campaigns.update")
        .unwrap();
    let forms_create = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.forms.create")
        .unwrap();
    let forms_update = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.forms.update")
        .unwrap();
    let forms_delete = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.forms.delete")
        .unwrap();
    let dashboard_read = permissions
        .iter()
        .find(|permission| permission["key"] == "crm.dashboard.read")
        .unwrap();
    assert_eq!(campaigns_create["crudActions"], json!(["create"]));
    assert_eq!(campaigns_update["crudActions"], json!(["update"]));
    assert_eq!(forms_create["crudActions"], json!(["create"]));
    assert_eq!(forms_update["crudActions"], json!(["update"]));
    assert_eq!(forms_delete["crudActions"], json!(["delete"]));
    assert_eq!(dashboard_read["crudActions"], json!(["read"]));

    let role_created = application
        .clone()
        .oneshot(
            Request::post("/api/v1/authorization/roles")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, &admin_authorization)
                .body(Body::from(
                    r#"{"key":"crm_reader","name":"CRM Reader","team":"Admissions","scope":"Read-only CRM access"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(role_created.status(), StatusCode::CREATED);
    let role_body: Value = serde_json::from_slice(
        &to_bytes(role_created.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let role_id = role_body["data"]["id"].as_str().unwrap();

    let user_created = application
        .clone()
        .oneshot(
            Request::post("/api/v1/authorization/users")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, &admin_authorization)
                .body(Body::from(format!(
                    r#"{{"name":"CRM Reader","email":"{reader_email}","temporaryPassword":"{reader_password}","roleIds":["{role_id}"]}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(user_created.status(), StatusCode::CREATED);

    let reader_login = application
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{reader_email}","password":"{reader_password}"}}"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader_login.status(), StatusCode::OK);
    let reader_body: Value = serde_json::from_slice(
        &to_bytes(reader_login.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let reader_authorization = format!(
        "Bearer {}",
        reader_body["data"]["accessToken"].as_str().unwrap()
    );

    let read_before_grant = application
        .clone()
        .oneshot(
            Request::get("/api/v1/crm/leads")
                .header(header::AUTHORIZATION, &reader_authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_before_grant.status(), StatusCode::FORBIDDEN);

    let grant_read = application
        .clone()
        .oneshot(
            Request::put(format!("/api/v1/authorization/roles/{role_id}/permissions"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, &admin_authorization)
                .body(Body::from(
                    r#"{"permissions":[{"key":"crm.leads.read","scope":"all","constraints":{}}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(grant_read.status(), StatusCode::OK);

    let read_after_grant = application
        .clone()
        .oneshot(
            Request::get("/api/v1/crm/leads")
                .header(header::AUTHORIZATION, &reader_authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_after_grant.status(), StatusCode::OK);

    let revoke_read = application
        .clone()
        .oneshot(
            Request::put(format!("/api/v1/authorization/roles/{role_id}/permissions"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, &admin_authorization)
                .body(Body::from(r#"{"permissions":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke_read.status(), StatusCode::OK);

    let read_after_revoke = application
        .clone()
        .oneshot(
            Request::get("/api/v1/crm/leads")
                .header(header::AUTHORIZATION, &reader_authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_after_revoke.status(), StatusCode::FORBIDDEN);

    sqlx::query("DELETE FROM platform.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(database.pool())
        .await
        .expect("clean RBAC tenant");
    sqlx::query("DELETE FROM identity.users WHERE email = ANY($1)")
        .bind(vec![admin_email, reader_email])
        .execute(database.pool())
        .await
        .expect("clean RBAC users");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL and writes temporary rows"]
async fn password_reset_replaces_the_password_and_revokes_existing_sessions() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.migrate().await.expect("database migration");

    let tenant = format!("reset-{}", Uuid::new_v4());
    let email = format!("reset-{}@supercampus.test", Uuid::new_v4());
    let old_password = "original-test-password";
    let new_password = "replacement-test-password";

    let tenant_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO platform.tenants (slug, code, name)
           VALUES ($1, $2, $3) RETURNING id"#,
    )
    .bind(&tenant)
    .bind(tenant.to_uppercase())
    .bind("Reset Test Campus")
    .fetch_one(database.pool())
    .await
    .expect("create tenant");
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO identity.users
           (email, password_hash, display_name, initials, account_type)
           VALUES ($1, crypt($2, gen_salt('bf', 4)), 'Reset User', 'RU', 'staff')
           RETURNING id"#,
    )
    .bind(&email)
    .bind(old_password)
    .fetch_one(database.pool())
    .await
    .expect("create identity");
    sqlx::query(
        r#"INSERT INTO identity.tenant_memberships
           (tenant_id, user_id, roles, is_primary, profile)
           VALUES ($1, $2, $3, true, '{}'::jsonb)"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(vec!["admissions_manager"])
    .execute(database.pool())
    .await
    .expect("create membership");

    let state = AppState::with_database(database.clone());
    let api = app(state.clone());

    // Sign in so there is a live session that the reset must revoke.
    let login = api
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{email}","password":"{old_password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), usize::MAX).await.unwrap()).unwrap();
    let session_id = body["data"]["sessionId"].as_str().unwrap().to_owned();

    // Request the reset through the public endpoint.
    let requested = api
        .clone()
        .oneshot(
            Request::post("/api/auth/forgot-password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"email":"{email}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(requested.status(), StatusCode::ACCEPTED);

    // The raw token is only in the email, so drive the state layer directly with a
    // token we plant ourselves using the same hashing the service uses.
    let raw_token = format!("test-{}", Uuid::new_v4());
    let token_hash: [u8; 32] = supercampus_authn::hash_refresh_token(&raw_token);
    sqlx::query(
        r#"INSERT INTO identity.password_reset_tokens (user_id, token_hash, expires_at)
           VALUES ($1, $2, now() + interval '1 hour')"#,
    )
    .bind(user_id)
    .bind(token_hash.to_vec())
    .execute(database.pool())
    .await
    .expect("plant reset token");

    let reset = api
        .clone()
        .oneshot(
            Request::post("/api/auth/reset-password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"token":"{raw_token}","password":"{new_password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::OK);

    // The same token cannot be replayed.
    let replay = api
        .clone()
        .oneshot(
            Request::post("/api/auth/reset-password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"token":"{raw_token}","password":"another-long-password"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    // The old password no longer works.
    let stale_login = api
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{email}","password":"{old_password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale_login.status(), StatusCode::UNAUTHORIZED);

    // The new password does.
    let fresh_login = api
        .clone()
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"email":"{email}","password":"{new_password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fresh_login.status(), StatusCode::OK);

    // The session that existed before the reset is revoked.
    let revoked: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM identity.auth_sessions WHERE id = $1::uuid")
            .bind(&session_id)
            .fetch_one(database.pool())
            .await
            .expect("read session");
    assert!(revoked.is_some(), "pre-reset session must be revoked");

    sqlx::query("DELETE FROM platform.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(database.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM identity.users WHERE id = $1")
        .bind(user_id)
        .execute(database.pool())
        .await
        .ok();
}
