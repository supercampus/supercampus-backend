use std::fs;

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::{get, post, put},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    state::{AppState, AuthPrincipal, EffectiveAccess, MINIMUM_PASSWORD_LENGTH},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/overview", get(overview))
        .route("/tenants", get(list_tenants).post(create_tenant))
        .route("/tenants/{tenant_id}", put(update_tenant))
        .route("/users", get(list_users))
        .route("/users/{user_id}", put(update_user))
        .route("/team", post(create_team_member))
        .route("/support", get(list_support).post(create_support))
        .route("/support/{ticket_id}", put(update_support))
        .route("/payments", get(list_payments))
        .route("/audit", get(list_audit))
}

fn require_platform_admin(access: &EffectiveAccess) -> ApiResult<()> {
    if access
        .roles
        .iter()
        .any(|role| role == "platform_super_admin")
        && access
            .portal_families
            .iter()
            .any(|family| family == "platform-control")
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn pool(state: &AppState) -> ApiResult<PgPool> {
    state
        .database()
        .map(|database| database.pool().clone())
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform storage is unavailable".into()))
}

async fn overview(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let pool = pool(&state)?;
    let row = sqlx::query(
        r#"SELECT
             (SELECT count(*) FROM platform.tenants WHERE slug <> 'supercampus-control') AS tenants,
             (SELECT count(*) FROM platform.tenants WHERE slug <> 'supercampus-control' AND status = 'active') AS active_tenants,
             (SELECT count(*) FROM identity.users) AS users,
             (SELECT count(*) FROM identity.users WHERE active) AS active_users,
             (SELECT count(*) FROM platform.support_tickets WHERE status IN ('open','in_progress','waiting')) AS open_support,
             (SELECT count(*) FROM platform.support_tickets WHERE priority = 'urgent' AND status NOT IN ('resolved','closed')) AS urgent_support,
             pg_database_size(current_database()) AS database_bytes,
             (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()) AS database_connections"#,
    )
    .fetch_one(&pool)
    .await?;
    let payment_summary = payment_summary(&state).await;
    let system = system_metrics();
    Ok(Json(ApiResponse::new(json!({
        "generatedAt": Utc::now(),
        "tenants": {"total": row.try_get::<i64, _>("tenants")?, "active": row.try_get::<i64, _>("active_tenants")?},
        "users": {"total": row.try_get::<i64, _>("users")?, "active": row.try_get::<i64, _>("active_users")?},
        "support": {"open": row.try_get::<i64, _>("open_support")?, "urgent": row.try_get::<i64, _>("urgent_support")?},
        "payments": {"paid": payment_summary.0, "capturedPaise": payment_summary.1},
        "database": {
            "status": "healthy",
            "sizeBytes": row.try_get::<i64, _>("database_bytes")?,
            "connections": row.try_get::<i64, _>("database_connections")?,
            "poolSize": pool.size(),
            "poolIdle": pool.num_idle()
        },
        "server": system
    }))))
}

fn system_metrics() -> Value {
    let load = fs::read_to_string("/proc/loadavg").ok().and_then(|value| {
        value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<f64>().ok())
    });
    let uptime_seconds = fs::read_to_string("/proc/uptime").ok().and_then(|value| {
        value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<f64>().ok())
    });
    let memory = fs::read_to_string("/proc/meminfo").ok().map(|contents| {
        let value = |key: &str| {
            contents.lines().find_map(|line| {
                let (name, rest) = line.split_once(':')?;
                (name == key)
                    .then(|| rest.split_whitespace().next()?.parse::<u64>().ok())
                    .flatten()
            })
        };
        let total = value("MemTotal").unwrap_or(0) * 1024;
        let available = value("MemAvailable").unwrap_or(0) * 1024;
        json!({"totalBytes": total, "usedBytes": total.saturating_sub(available)})
    });
    json!({
        "status": "healthy",
        "loadAverage1m": load,
        "uptimeSeconds": uptime_seconds,
        "memory": memory,
        "observedAt": Utc::now()
    })
}

async fn list_tenants(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let rows = sqlx::query(
        r#"SELECT tenant.id, tenant.slug, tenant.code, tenant.name, tenant.city, tenant.status,
                  tenant.created_at,
                  (SELECT count(*) FROM identity.tenant_memberships membership WHERE membership.tenant_id = tenant.id AND membership.active) AS users,
                  (SELECT count(*) FROM platform.support_tickets ticket WHERE ticket.tenant_id = tenant.id AND ticket.status NOT IN ('resolved','closed')) AS open_support
           FROM platform.tenants tenant
           WHERE tenant.slug <> 'supercampus-control'
           ORDER BY tenant.created_at DESC"#,
    )
    .fetch_all(&pool(&state)?)
    .await?;
    let tenants = rows
        .iter()
        .map(|row| json!({
            "id": row.get::<Uuid, _>("id"), "slug": row.get::<String, _>("slug"),
            "code": row.get::<String, _>("code"), "name": row.get::<String, _>("name"),
            "city": row.get::<String, _>("city"), "status": row.get::<String, _>("status"),
            "createdAt": row.get::<chrono::DateTime<Utc>, _>("created_at"),
            "users": row.get::<i64, _>("users"), "openSupport": row.get::<i64, _>("open_support")
        }))
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"tenants": tenants}))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTenantRequest {
    name: String,
    slug: String,
    code: String,
    #[serde(default)]
    city: String,
    admin_name: String,
    admin_email: String,
    admin_password: String,
}

async fn create_tenant(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Extension(principal): Extension<AuthPrincipal>,
    Json(request): Json<CreateTenantRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<ApiResponse<Value>>)> {
    require_platform_admin(&access)?;
    validate_slug(&request.slug)?;
    validate_email_password(&request.admin_email, &request.admin_password)?;
    if request.name.trim().is_empty()
        || request.code.trim().is_empty()
        || request.admin_name.trim().is_empty()
    {
        return Err(ApiError::BadRequest(
            "Name, code and administrator name are required".into(),
        ));
    }
    let pool = pool(&state)?;
    let mut transaction = pool.begin().await?;
    let tenant_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO platform.tenants (slug, code, name, city, status)
           VALUES ($1, $2, $3, $4, 'active') RETURNING id"#,
    )
    .bind(request.slug.trim().to_ascii_lowercase())
    .bind(request.code.trim().to_ascii_uppercase())
    .bind(request.name.trim())
    .bind(request.city.trim())
    .fetch_one(&mut *transaction)
    .await
    .map_err(conflict_or_internal)?;
    let email = request.admin_email.trim().to_ascii_lowercase();
    let user_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO identity.users
           (email, password_hash, display_name, initials, account_type, active)
           VALUES ($1, crypt($2, gen_salt('bf', 12)), $3, $4, 'staff', true)
           RETURNING id"#,
    )
    .bind(&email)
    .bind(&request.admin_password)
    .bind(request.admin_name.trim())
    .bind(initials(&request.admin_name))
    .fetch_one(&mut *transaction)
    .await
    .map_err(conflict_or_internal)?;
    sqlx::query(
        r#"INSERT INTO identity.tenant_memberships (tenant_id, user_id, roles, active, is_primary)
           VALUES ($1, $2, ARRAY['tenant_admin']::text[], true, true)"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
           SELECT $1, $2, id, $3 FROM authz.roles
           WHERE tenant_id = $1 AND role_key = 'tenant_admin'"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&principal.student.id)
    .execute(&mut *transaction)
    .await?;
    record_audit(
        &mut transaction,
        tenant_id,
        &principal,
        "platform.tenant.created",
        "tenant",
        &tenant_id.to_string(),
    )
    .await?;
    transaction.commit().await?;
    let database_name = format!("supercampus_{}", request.slug.trim().replace('-', "_"));
    let databases = state.tenant_databases().ok_or_else(|| {
        ApiError::ServiceUnavailable("Tenant database manager is unavailable".into())
    })?;
    if let Err(error) = databases
        .provision(request.slug.trim(), &database_name)
        .await
    {
        tracing::error!(tenant_slug = request.slug.trim(), error = ?error, "tenant database provisioning failed");
        sqlx::query("UPDATE platform.tenants SET status='failed', updated_at=now() WHERE id=$1")
            .bind(tenant_id)
            .execute(&pool)
            .await?;
        return Err(ApiError::ServiceUnavailable(
            "The tenant record was created, but its database could not be provisioned. Retry after checking database permissions".into(),
        ));
    }
    Ok((
        axum::http::StatusCode::CREATED,
        Json(ApiResponse::new(json!({
            "tenant": {"id": tenant_id, "slug": request.slug.trim().to_ascii_lowercase(), "name": request.name.trim(), "status": "active"},
            "administrator": {"id": user_id, "email": email, "name": request.admin_name.trim()}
        }))),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateTenantRequest {
    status: String,
}

async fn update_tenant(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Extension(principal): Extension<AuthPrincipal>,
    Path(tenant_id): Path<Uuid>,
    Json(request): Json<UpdateTenantRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    if !matches!(request.status.as_str(), "active" | "suspended") {
        return Err(ApiError::BadRequest(
            "Status must be active or suspended".into(),
        ));
    }
    let pool = pool(&state)?;
    let result = sqlx::query(
        "UPDATE platform.tenants SET status=$2, updated_at=now() WHERE id=$1 AND slug <> 'supercampus-control'",
    )
    .bind(tenant_id)
    .bind(&request.status)
    .execute(&pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Tenant not found".into()));
    }
    let mut transaction = pool.begin().await?;
    record_audit(
        &mut transaction,
        tenant_id,
        &principal,
        "platform.tenant.status_changed",
        "tenant",
        &tenant_id.to_string(),
    )
    .await?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(
        json!({"id": tenant_id, "status": request.status}),
    )))
}

async fn list_users(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let rows = sqlx::query(
        r#"SELECT users.id, users.email, users.display_name, users.account_type, users.active,
                  users.last_login_at, users.created_at,
                  COALESCE(jsonb_agg(DISTINCT jsonb_build_object(
                    'id', tenant.id, 'name', tenant.name, 'slug', tenant.slug,
                    'roles', membership.roles, 'active', membership.active
                  )) FILTER (WHERE tenant.id IS NOT NULL), '[]'::jsonb) AS memberships
           FROM identity.users users
           LEFT JOIN identity.tenant_memberships membership ON membership.user_id = users.id
           LEFT JOIN platform.tenants tenant ON tenant.id = membership.tenant_id
           GROUP BY users.id
           ORDER BY users.created_at DESC LIMIT 500"#,
    )
    .fetch_all(&pool(&state)?)
    .await?;
    let users = rows.iter().map(|row| json!({
        "id": row.get::<Uuid, _>("id"), "email": row.get::<String, _>("email"),
        "name": row.get::<String, _>("display_name"), "accountType": row.get::<String, _>("account_type"),
        "active": row.get::<bool, _>("active"), "lastLoginAt": row.get::<Option<chrono::DateTime<Utc>>, _>("last_login_at"),
        "createdAt": row.get::<chrono::DateTime<Utc>, _>("created_at"), "memberships": row.get::<Value, _>("memberships")
    })).collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"users": users}))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateUserRequest {
    active: bool,
}

async fn update_user(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Extension(principal): Extension<AuthPrincipal>,
    Path(user_id): Path<Uuid>,
    Json(request): Json<UpdateUserRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    if !request.active && Uuid::parse_str(&principal.student.id).ok() == Some(user_id) {
        return Err(ApiError::BadRequest(
            "You cannot deactivate your own platform account".into(),
        ));
    }
    let result = sqlx::query("UPDATE identity.users SET active=$2, updated_at=now() WHERE id=$1")
        .bind(user_id)
        .bind(request.active)
        .execute(&pool(&state)?)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("User not found".into()));
    }
    Ok(Json(ApiResponse::new(
        json!({"id": user_id, "active": request.active}),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTeamMemberRequest {
    name: String,
    email: String,
    password: String,
}

async fn create_team_member(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Extension(principal): Extension<AuthPrincipal>,
    Json(request): Json<CreateTeamMemberRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<ApiResponse<Value>>)> {
    require_platform_admin(&access)?;
    validate_email_password(&request.email, &request.password)?;
    if request.name.trim().is_empty() {
        return Err(ApiError::BadRequest("Name is required".into()));
    }
    let pool = pool(&state)?;
    let mut transaction = pool.begin().await?;
    let tenant_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM platform.tenants WHERE slug='supercampus-control'",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let email = request.email.trim().to_ascii_lowercase();
    let user_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO identity.users (email,password_hash,display_name,initials,account_type,active)
           VALUES ($1,crypt($2,gen_salt('bf',12)),$3,$4,'staff',true) RETURNING id"#)
        .bind(&email).bind(&request.password).bind(request.name.trim()).bind(initials(&request.name))
        .fetch_one(&mut *transaction).await.map_err(conflict_or_internal)?;
    sqlx::query("INSERT INTO identity.tenant_memberships (tenant_id,user_id,roles,active,is_primary) VALUES ($1,$2,ARRAY['platform_super_admin']::text[],true,true)")
        .bind(tenant_id).bind(user_id).execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO authz.user_roles (tenant_id,user_id,role_id,assigned_by) SELECT $1,$2,id,$3 FROM authz.roles WHERE tenant_id=$1 AND role_key='platform_super_admin'")
        .bind(tenant_id).bind(user_id).bind(&principal.student.id).execute(&mut *transaction).await?;
    record_audit(
        &mut transaction,
        tenant_id,
        &principal,
        "platform.team.created",
        "user",
        &user_id.to_string(),
    )
    .await?;
    transaction.commit().await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(ApiResponse::new(
            json!({"id": user_id, "email": email, "name": request.name.trim()}),
        )),
    ))
}

async fn list_support(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let rows = sqlx::query(
        r#"SELECT ticket.id, ticket.subject, ticket.description, ticket.requester_name, ticket.requester_email,
                  ticket.priority, ticket.status, ticket.created_at, ticket.updated_at,
                  tenant.id AS tenant_id, tenant.name AS tenant_name
           FROM platform.support_tickets ticket LEFT JOIN platform.tenants tenant ON tenant.id=ticket.tenant_id
           ORDER BY CASE ticket.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,
                    ticket.created_at DESC LIMIT 300"#)
        .fetch_all(&pool(&state)?).await?;
    let tickets = rows.iter().map(|row| json!({
        "id": row.get::<Uuid,_>("id"), "subject": row.get::<String,_>("subject"), "description": row.get::<String,_>("description"),
        "requesterName": row.get::<String,_>("requester_name"), "requesterEmail": row.get::<String,_>("requester_email"),
        "priority": row.get::<String,_>("priority"), "status": row.get::<String,_>("status"),
        "tenantId": row.get::<Option<Uuid>,_>("tenant_id"), "tenantName": row.get::<Option<String>,_>("tenant_name"),
        "createdAt": row.get::<chrono::DateTime<Utc>,_>("created_at"), "updatedAt": row.get::<chrono::DateTime<Utc>,_>("updated_at")
    })).collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"tickets": tickets}))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSupportRequest {
    tenant_id: Option<Uuid>,
    subject: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    requester_name: String,
    #[serde(default)]
    requester_email: String,
    #[serde(default = "normal_priority")]
    priority: String,
}
fn normal_priority() -> String {
    "normal".into()
}

async fn create_support(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateSupportRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<ApiResponse<Value>>)> {
    require_platform_admin(&access)?;
    validate_priority(&request.priority)?;
    if request.subject.trim().is_empty() {
        return Err(ApiError::BadRequest("Subject is required".into()));
    }
    let id = sqlx::query_scalar::<_,Uuid>("INSERT INTO platform.support_tickets (tenant_id,subject,description,requester_name,requester_email,priority) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(request.tenant_id).bind(request.subject.trim()).bind(request.description.trim()).bind(request.requester_name.trim()).bind(request.requester_email.trim()).bind(&request.priority)
        .fetch_one(&pool(&state)?).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(ApiResponse::new(json!({"id": id, "status": "open"}))),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSupportRequest {
    status: String,
    priority: String,
}

async fn update_support(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
    Path(ticket_id): Path<Uuid>,
    Json(request): Json<UpdateSupportRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    validate_priority(&request.priority)?;
    if !matches!(
        request.status.as_str(),
        "open" | "in_progress" | "waiting" | "resolved" | "closed"
    ) {
        return Err(ApiError::BadRequest("Invalid support status".into()));
    }
    let result = sqlx::query("UPDATE platform.support_tickets SET status=$2,priority=$3,updated_at=now(),resolved_at=CASE WHEN $2 IN ('resolved','closed') THEN now() ELSE NULL END WHERE id=$1")
        .bind(ticket_id).bind(&request.status).bind(&request.priority).execute(&pool(&state)?).await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Support ticket not found".into()));
    }
    Ok(Json(ApiResponse::new(
        json!({"id": ticket_id,"status":request.status,"priority":request.priority}),
    )))
}

async fn list_payments(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let mut payments = Vec::new();
    let mut unavailable_tenants = Vec::new();
    for slug in state.registered_tenant_slugs().await? {
        let tenant_database = match state.tenant_database(&slug).await {
            Ok(database) => database,
            Err(error) => {
                tracing::warn!(tenant_slug = slug, error = ?error, "platform payment aggregation skipped an unavailable tenant");
                unavailable_tenants.push(slug);
                continue;
            }
        };
        let rows = sqlx::query(
        r#"SELECT payment.id,payment.amount_paise,payment.currency,payment.status,payment.student_number,
                  payment.guardian_name,payment.provider_link_id,payment.created_at,payment.paid_at,
                  tenant.id AS tenant_id,tenant.name AS tenant_name
           FROM campus_ops.guardian_fee_payment_links payment
           JOIN platform.tenants tenant ON tenant.id=payment.tenant_id
           ORDER BY payment.created_at DESC LIMIT 300"#).fetch_all(tenant_database.pool()).await?;
        payments.extend(rows.iter().map(|row| json!({
            "id":row.get::<Uuid,_>("id"),"amountPaise":row.get::<i64,_>("amount_paise"),"currency":row.get::<String,_>("currency"),
            "status":row.get::<String,_>("status"),"studentNumber":row.get::<String,_>("student_number"),"guardianName":row.get::<String,_>("guardian_name"),
            "providerReference":row.get::<String,_>("provider_link_id"),"tenantId":row.get::<Uuid,_>("tenant_id"),"tenantName":row.get::<String,_>("tenant_name"),
            "createdAt":row.get::<chrono::DateTime<Utc>,_>("created_at"),"paidAt":row.get::<Option<chrono::DateTime<Utc>>,_>("paid_at")
        })));
    }
    payments.sort_by(|left, right| right["createdAt"].as_str().cmp(&left["createdAt"].as_str()));
    payments.truncate(300);
    Ok(Json(ApiResponse::new(
        json!({"payments":payments, "unavailableTenants": unavailable_tenants}),
    )))
}

async fn payment_summary(state: &AppState) -> (i64, i64) {
    let Ok(slugs) = state.registered_tenant_slugs().await else {
        return (0, 0);
    };
    let mut paid = 0_i64;
    let mut amount = 0_i64;
    for slug in slugs {
        let Ok(database) = state.tenant_database(&slug).await else {
            continue;
        };
        if let Ok(row) = sqlx::query(
            "SELECT count(*) AS paid, COALESCE(sum(amount_paise),0) AS amount FROM campus_ops.guardian_fee_payment_links WHERE status='paid'",
        )
        .fetch_one(database.pool())
        .await
        {
            paid += row.get::<i64, _>("paid");
            amount += row.get::<i64, _>("amount");
        }
    }
    (paid, amount)
}

async fn list_audit(
    State(state): State<AppState>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_platform_admin(&access)?;
    let rows=sqlx::query(
        r#"SELECT entry.id,entry.action,entry.resource_type,entry.resource_id,entry.after_value,entry.occurred_at,
                  tenant.name AS tenant_name,user_identity.display_name AS actor_name
           FROM audit.entries entry JOIN platform.tenants tenant ON tenant.id=entry.tenant_id
           LEFT JOIN identity.users user_identity ON user_identity.id=entry.actor_id
           ORDER BY entry.occurred_at DESC LIMIT 300"#).fetch_all(&pool(&state)?).await?;
    let events=rows.iter().map(|row| json!({"id":row.get::<Uuid,_>("id"),"action":row.get::<String,_>("action"),"resourceType":row.get::<String,_>("resource_type"),"resourceId":row.get::<Option<String>,_>("resource_id"),"details":row.get::<Option<Value>,_>("after_value"),"tenantName":row.get::<String,_>("tenant_name"),"actorName":row.get::<Option<String>,_>("actor_name"),"occurredAt":row.get::<chrono::DateTime<Utc>,_>("occurred_at")})).collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"events":events}))))
}

fn validate_slug(value: &str) -> ApiResult<()> {
    let value = value.trim();
    if value.len() < 2
        || value.len() > 60
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(ApiError::BadRequest(
            "Slug must use lowercase letters, numbers and single hyphens".into(),
        ));
    }
    Ok(())
}
fn validate_email_password(email: &str, password: &str) -> ApiResult<()> {
    if !email.contains('@') || email.trim() != email || email.to_ascii_lowercase() != email {
        return Err(ApiError::BadRequest(
            "Enter a lowercase email address".into(),
        ));
    }
    if password.chars().count() < MINIMUM_PASSWORD_LENGTH {
        return Err(ApiError::BadRequest(format!(
            "Password must contain at least {MINIMUM_PASSWORD_LENGTH} characters"
        )));
    }
    Ok(())
}
fn validate_priority(value: &str) -> ApiResult<()> {
    if matches!(value, "low" | "normal" | "high" | "urgent") {
        Ok(())
    } else {
        Err(ApiError::BadRequest("Invalid priority".into()))
    }
}
fn initials(value: &str) -> String {
    let result = value
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_ascii_uppercase();
    if result.is_empty() {
        "SC".into()
    } else {
        result
    }
}
fn conflict_or_internal(error: sqlx::Error) -> ApiError {
    if error
        .as_database_error()
        .is_some_and(|db| db.is_unique_violation())
    {
        ApiError::Conflict("The tenant code, slug or administrator email already exists".into())
    } else {
        ApiError::from(error)
    }
}

async fn record_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    principal: &AuthPrincipal,
    action: &str,
    resource_type: &str,
    resource_id: &str,
) -> ApiResult<()> {
    let actor_id = Uuid::parse_str(&principal.student.id).ok();
    sqlx::query("INSERT INTO audit.entries (tenant_id,actor_id,action,resource_type,resource_id,after_value) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(tenant_id).bind(actor_id).bind(action).bind(resource_type).bind(resource_id).bind(json!({"actorEmail":principal.student.email})).execute(&mut **transaction).await?;
    Ok(())
}

pub async fn seed_platform_admin_from_environment(state: &AppState) -> anyhow::Result<bool> {
    let (email, password) = match (
        std::env::var("PLATFORM_ADMIN_EMAIL"),
        std::env::var("PLATFORM_ADMIN_PASSWORD"),
    ) {
        (Ok(email), Ok(password)) if !email.trim().is_empty() && !password.is_empty() => {
            (email.trim().to_ascii_lowercase(), password)
        }
        _ => return Ok(false),
    };
    if password.chars().count() < MINIMUM_PASSWORD_LENGTH {
        anyhow::bail!(
            "PLATFORM_ADMIN_PASSWORD must contain at least {MINIMUM_PASSWORD_LENGTH} characters"
        );
    }
    let name =
        std::env::var("PLATFORM_ADMIN_NAME").unwrap_or_else(|_| "SuperCampus Operator".into());
    let database = state
        .database()
        .ok_or_else(|| anyhow::anyhow!("platform administrator seeding requires PostgreSQL"))?;
    let mut transaction = database.pool().begin().await?;
    let tenant_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM platform.tenants WHERE slug='supercampus-control'",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let existing_user = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT users.id
           FROM identity.users users
           JOIN identity.tenant_memberships membership
             ON membership.user_id = users.id AND membership.tenant_id = $2
           JOIN authz.user_roles user_role
             ON user_role.user_id = users.id AND user_role.tenant_id = $2
           JOIN authz.roles role
             ON role.id = user_role.role_id AND role.role_key = 'platform_super_admin'
           WHERE users.email = $1"#,
    )
    .bind(&email)
    .bind(tenant_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let user_id = if let Some(user_id) = existing_user {
        sqlx::query(
            r#"UPDATE identity.users
               SET password_hash=crypt($2,gen_salt('bf',12)), display_name=$3,
                   initials=$4, active=true, updated_at=now()
               WHERE id=$1"#,
        )
        .bind(user_id)
        .bind(&password)
        .bind(name.trim())
        .bind(initials(&name))
        .execute(&mut *transaction)
        .await?;
        user_id
    } else {
        let conflicting_user = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM identity.users WHERE email=$1)",
        )
        .bind(&email)
        .fetch_one(&mut *transaction)
        .await?;
        if conflicting_user {
            anyhow::bail!("PLATFORM_ADMIN_EMAIL already belongs to a non-platform account");
        }
        sqlx::query_scalar::<_, Uuid>(
            r#"INSERT INTO identity.users
               (email,password_hash,display_name,initials,account_type,active)
               VALUES ($1,crypt($2,gen_salt('bf',12)),$3,$4,'staff',true)
               RETURNING id"#,
        )
        .bind(&email)
        .bind(&password)
        .bind(name.trim())
        .bind(initials(&name))
        .fetch_one(&mut *transaction)
        .await?
    };
    sqlx::query("INSERT INTO identity.tenant_memberships (tenant_id,user_id,roles,active,is_primary) VALUES ($1,$2,ARRAY['platform_super_admin']::text[],true,true) ON CONFLICT(tenant_id,user_id) DO UPDATE SET roles=EXCLUDED.roles,active=true,is_primary=true,updated_at=now()")
        .bind(tenant_id).bind(user_id).execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO authz.user_roles (tenant_id,user_id,role_id,assigned_by) SELECT $1,$2,id,'platform-admin-environment' FROM authz.roles WHERE tenant_id=$1 AND role_key='platform_super_admin' ON CONFLICT DO NOTHING")
        .bind(tenant_id).bind(user_id).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_platform_slugs() {
        assert!(validate_slug("madras-engineering-college").is_ok());
        assert!(validate_slug("Madras College").is_err());
    }
    #[test]
    fn builds_initials() {
        assert_eq!(initials("SuperCampus Operator"), "SO");
    }
    #[test]
    fn platform_role_requires_platform_family() {
        let access = EffectiveAccess {
            roles: vec!["platform_super_admin".into()],
            portal_families: vec!["admin".into()],
            permissions: vec!["*".into()],
            scopes: Default::default(),
        };
        assert!(require_platform_admin(&access).is_err());
    }
}
