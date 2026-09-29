//! Ad-hoc payment requests and online (Razorpay) payment tracking.
//!
//! The accounts office asks students to pay a fixed amount for a purpose (a
//! fine, an electricity bill, ...). Every targeted student gets a payer row
//! that a student settles through Razorpay checkout or the office marks paid
//! (cash, bank transfer, ...). Students see their open requests on the home
//! screen through `GET /payment-requests/mine`.
//!
//! Online Payments reads `campus_ops.razorpay_orders`, the ledger the checkout
//! endpoints write, plus older wallet/tuition/payment-link records that
//! predate the ledger. A reconciliation run reads order payments and
//! settlements back from Razorpay; nothing about capture or settlement is
//! inferred locally.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    notification::{NotificationSpec, Recipient, enqueue_tx},
    operations::{require, require_any, tenant_id},
    razorpay::{PaymentOwner, RazorpayOrder},
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

const READ: &str = "fees.payment_requests.read";
const MANAGE: &str = "fees.payment_requests.manage";
const ONLINE_READ: &str = "fees.online_payments.read";
const ONLINE_RECONCILE: &str = "fees.online_payments.reconcile";

/// Purposes a request can be raised for, with their display labels.
pub(crate) const PURPOSES: &[(&str, &str)] = &[
    ("fine", "Fine"),
    ("electricity", "Electricity bill"),
    ("hostel", "Hostel"),
    ("exam", "Examination"),
    ("library", "Library"),
    ("transport", "Transport"),
    ("event", "Event"),
    ("other", "Other"),
];

/// Ways the office can record a payment it received outside Razorpay.
const MANUAL_METHODS: &[(&str, &str)] = &[
    ("cash", "Cash"),
    ("bank_transfer", "Bank transfer"),
    ("upi", "UPI (outside the app)"),
    ("cheque", "Cheque"),
    ("other", "Other"),
];

const MAX_AMOUNT: f64 = 1_000_000.0;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/payment-requests", get(list_requests).post(create_request))
        .route("/payment-requests/mine", get(my_requests))
        .route("/payment-requests/options", get(request_options))
        .route("/payment-requests/students", get(search_students))
        .route("/payment-requests/{request_id}", get(request_detail))
        .route(
            "/payment-requests/{request_id}/cancel",
            post(cancel_request),
        )
        .route(
            "/payment-requests/{request_id}/payers/{payer_id}/mark-paid",
            post(mark_paid_manually),
        )
        .route("/online-payments", get(online_payments))
        .route("/online-payments/sync", post(sync_online_payments))
}

// ------------------------------------------------------------------ schema

/// Creates the payment request tables and the Razorpay order ledger.
/// Mirrors migrations/runtime/0120; DDL runs once per tenant per process.
pub(crate) async fn ensure_schema(pool: &sqlx::PgPool, tenant_key: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static READY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let ready = READY.get_or_init(|| Mutex::new(HashSet::new()));
    if ready
        .lock()
        .map(|set| set.contains(tenant_key))
        .unwrap_or(false)
    {
        return;
    }
    let applied = sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0120_payment_requests_and_online_payments.sql"
    ))
    .execute(pool)
    .await;
    match applied {
        Ok(_) => {
            if let Ok(mut set) = ready.lock() {
                set.insert(tenant_key.to_owned());
            }
        }
        Err(error) => {
            tracing::warn!(%error, tenant = tenant_key, "payment request schema not applied");
        }
    }
}

/// Registers the payment request / online payment permissions and grants
/// them to the accounts office. Runs at startup against the authorization
/// database; mirrors migrations/runtime/0121.
pub async fn ensure_permissions(pool: &sqlx::PgPool) {
    if let Err(error) = sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0121_payment_request_permissions.sql"
    ))
    .execute(pool)
    .await
    {
        tracing::warn!(%error, "payment request permissions not applied");
    }
}

// ------------------------------------------------------------------ validation

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRequestInput {
    purpose: Option<String>,
    title: Option<String>,
    description: Option<String>,
    amount: Option<f64>,
    due_date: Option<String>,
    show_on_dashboard: Option<bool>,
    /// all | students | cohort
    target: Option<String>,
    #[serde(default)]
    student_user_ids: Vec<String>,
    #[serde(default)]
    departments: Vec<String>,
    #[serde(default)]
    years: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Target {
    All,
    Students(Vec<String>),
    Cohort {
        departments: Vec<String>,
        years: Vec<String>,
    },
}

impl Target {
    fn mode(&self) -> &'static str {
        match self {
            Target::All => "all",
            Target::Students(_) => "students",
            Target::Cohort { .. } => "cohort",
        }
    }
}

#[derive(Debug, PartialEq)]
struct NewRequest {
    purpose: &'static str,
    title: String,
    description: String,
    amount: f64,
    due_date: Option<NaiveDate>,
    show_on_dashboard: bool,
    target: Target,
}

fn purpose_key(value: Option<&str>) -> Option<&'static str> {
    let value = value.unwrap_or("other").trim().to_ascii_lowercase();
    PURPOSES
        .iter()
        .find(|(key, _)| *key == value)
        .map(|(key, _)| *key)
}

pub(crate) fn purpose_label(key: &str) -> &'static str {
    PURPOSES
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, label)| *label)
        .unwrap_or("Other")
}

fn clean_list(values: &[String]) -> Vec<String> {
    let mut cleaned: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    cleaned.sort();
    cleaned.dedup();
    cleaned
}

fn validate_new_request(input: CreateRequestInput, today: NaiveDate) -> Result<NewRequest, String> {
    let purpose = purpose_key(input.purpose.as_deref()).ok_or("Choose a valid purpose")?;
    let title = input.title.unwrap_or_default().trim().to_owned();
    if title.chars().count() < 3 {
        return Err("Title must be at least 3 characters".into());
    }
    if title.chars().count() > 120 {
        return Err("Title must be 120 characters or fewer".into());
    }
    let description = input.description.unwrap_or_default().trim().to_owned();
    if description.chars().count() < 10 {
        return Err("Description must be at least 10 characters".into());
    }
    if description.chars().count() > 1000 {
        return Err("Description must be 1000 characters or fewer".into());
    }
    let amount = input
        .amount
        .filter(|amount| amount.is_finite())
        .ok_or("Enter an amount")?;
    let amount = (amount * 100.0).round() / 100.0;
    if amount < 1.0 {
        return Err("Amount must be at least ₹1".into());
    }
    if amount > MAX_AMOUNT {
        return Err("Amount must be ₹10,00,000 or less".into());
    }
    let due_date = match input.due_date.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) => {
            let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| "Due date must be YYYY-MM-DD".to_owned())?;
            if date < today {
                return Err("Due date cannot be in the past".into());
            }
            Some(date)
        }
    };
    let target = match input.target.as_deref().unwrap_or("all").trim() {
        "all" => Target::All,
        "students" => {
            let ids = clean_list(&input.student_user_ids);
            if ids.is_empty() {
                return Err("Choose at least one student".into());
            }
            if ids.len() > 2000 {
                return Err("Choose 2000 students or fewer".into());
            }
            Target::Students(ids)
        }
        "cohort" => {
            let departments = clean_list(&input.departments);
            let years = clean_list(&input.years);
            if departments.is_empty() && years.is_empty() {
                return Err("Choose a department or a year".into());
            }
            Target::Cohort { departments, years }
        }
        _ => return Err("Target must be all, students or cohort".into()),
    };
    Ok(NewRequest {
        purpose,
        title,
        description,
        amount,
        due_date,
        show_on_dashboard: input.show_on_dashboard.unwrap_or(true),
        target,
    })
}

/// pending | overdue | paid | cancelled as a student and the office see it.
fn display_status(status: &str, due_date: Option<NaiveDate>, today: NaiveDate) -> &'static str {
    match status {
        "paid" => "paid",
        "cancelled" => "cancelled",
        _ if due_date.is_some_and(|due| due < today) => "overdue",
        _ => "pending",
    }
}

fn campus_today() -> NaiveDate {
    // Campus dates are Indian Standard Time (UTC+05:30).
    (Utc::now() + Duration::minutes(330)).date_naive()
}

fn to_paise(amount: f64) -> i64 {
    (amount * 100.0).round() as i64
}

fn publish(state: &AppState, tenant_slug: &str, event: &str, resource: &str, id: &str) {
    state.publish_realtime(RealtimePublication::tenant(
        tenant_slug,
        format!("payments.{event}"),
        json!({
            "module": "payments",
            "resource": resource,
            "resourceId": id,
            "operation": event,
            "invalidate": true,
        }),
    ));
}

#[allow(clippy::too_many_arguments)]
async fn record_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    aggregate: &str,
    id: &str,
    event: &str,
    actor: &str,
    payload: &Value,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO campus_ops.events(tenant_id,module_key,aggregate_type,aggregate_id,event_type,actor_user_id,payload) VALUES($1,'payments',$2,$3,$4,$5,$6)",
    )
    .bind(tenant)
    .bind(aggregate)
    .bind(id)
    .bind(event)
    .bind(actor)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn notify_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    user_id: &str,
    event_type: &str,
    title: &str,
    body: &str,
    data: Value,
    dedupe: String,
) -> ApiResult<()> {
    enqueue_tx(
        tx,
        tenant,
        NotificationSpec {
            recipient: Recipient::User(user_id.to_owned()),
            category: "fees".into(),
            event_type: event_type.into(),
            title: title.into(),
            body: body.into(),
            data,
            priority: "high".into(),
            requires_action: event_type == "payment_request.created",
            deep_link: None,
            deduplication_key: Some(dedupe),
            expires_at: None,
        },
    )
    .await
}

fn format_rupees(amount: f64) -> String {
    if (amount - amount.round()).abs() < 0.005 {
        format!("₹{}", amount.round() as i64)
    } else {
        format!("₹{amount:.2}")
    }
}

// ------------------------------------------------------------------ admin

#[derive(Debug, Deserialize)]
struct ListQuery {
    status: Option<String>,
}

fn request_json(row: &sqlx::postgres::PgRow, today: NaiveDate) -> Value {
    let purpose: String = row.get("purpose");
    let due_date: Option<NaiveDate> = row.get("due_date");
    let payer_count: i64 = row.try_get("payer_count").unwrap_or(0);
    let paid_count: i64 = row.try_get("paid_count").unwrap_or(0);
    let pending_count: i64 = row.try_get("pending_count").unwrap_or(0);
    let status: String = row.get("status");
    let overdue = status == "active" && pending_count > 0 && due_date.is_some_and(|d| d < today);
    json!({
        "id": row.get::<Uuid, _>("id"),
        "purpose": purpose,
        "purposeLabel": purpose_label(&purpose),
        "title": row.get::<String, _>("title"),
        "description": row.get::<String, _>("description"),
        "amount": row.get::<f64, _>("amount"),
        "currency": row.get::<String, _>("currency"),
        "dueDate": due_date,
        "showOnDashboard": row.get::<bool, _>("show_on_dashboard"),
        "targetMode": row.get::<String, _>("target_mode"),
        "targetDepartments": row.get::<Vec<String>, _>("target_departments"),
        "targetYears": row.get::<Vec<String>, _>("target_years"),
        "status": status,
        "overdue": overdue,
        "createdBy": row.get::<String, _>("created_by"),
        "createdByName": row.get::<Option<String>, _>("created_by_name"),
        "cancelReason": row.get::<Option<String>, _>("cancel_reason"),
        "cancelledAt": row.get::<Option<DateTime<Utc>>, _>("cancelled_at"),
        "createdAt": row.get::<DateTime<Utc>, _>("created_at"),
        "payerCount": payer_count,
        "paidCount": paid_count,
        "pendingCount": pending_count,
        "collectedAmount": row.try_get::<f64, _>("collected_amount").unwrap_or(0.0),
        "expectedAmount": row.try_get::<f64, _>("expected_amount").unwrap_or(0.0),
    })
}

const REQUEST_COLUMNS: &str = r#"request.id, request.purpose, request.title, request.description,
    request.amount::float8 AS amount, request.currency, request.due_date, request.show_on_dashboard,
    request.target_mode, request.target_departments, request.target_years, request.status,
    request.created_by, request.created_by_name, request.cancel_reason, request.cancelled_at,
    request.created_at,
    (SELECT count(*) FROM campus_ops.payment_request_payers p
       WHERE p.request_id=request.id AND p.status<>'cancelled') AS payer_count,
    (SELECT count(*) FROM campus_ops.payment_request_payers p
       WHERE p.request_id=request.id AND p.status='paid') AS paid_count,
    (SELECT count(*) FROM campus_ops.payment_request_payers p
       WHERE p.request_id=request.id AND p.status='pending') AS pending_count,
    (SELECT COALESCE(sum(p.amount),0)::float8 FROM campus_ops.payment_request_payers p
       WHERE p.request_id=request.id AND p.status='paid') AS collected_amount,
    (SELECT COALESCE(sum(p.amount),0)::float8 FROM campus_ops.payment_request_payers p
       WHERE p.request_id=request.id AND p.status<>'cancelled') AS expected_amount"#;

async fn list_requests(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, &[READ, MANAGE])?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    let status = match query.status.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some(value @ ("active" | "closed" | "cancelled")) => Some(value.to_owned()),
        Some(_) => return Err(ApiError::BadRequest("Unknown status filter".into())),
    };
    let rows = sqlx::query(&format!(
        r#"SELECT {REQUEST_COLUMNS}
           FROM campus_ops.payment_requests request
           WHERE request.tenant_id=$1 AND ($2::text IS NULL OR request.status=$2)
           ORDER BY (request.status='active') DESC, request.created_at DESC
           LIMIT 300"#
    ))
    .bind(tenant)
    .bind(&status)
    .fetch_all(db.pool())
    .await?;
    let today = campus_today();
    let requests: Vec<Value> = rows.iter().map(|row| request_json(row, today)).collect();
    Ok(Json(ApiResponse::new(json!({
        "requests": requests,
        "canManage": access.allows(MANAGE),
    }))))
}

async fn request_options(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, MANAGE)?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    let tenant = tenant_id(db.pool(), slug).await?;
    let cohorts = sqlx::query_as::<_, (String, String, i64)>(&format!(
        r#"SELECT {DEPARTMENT_SQL} AS department, {YEAR_SQL} AS year, count(*)
           FROM core.students student
           LEFT JOIN core.departments department
             ON department.tenant_id=student.tenant_id AND department.id::text=student.department_id
           WHERE student.tenant_id=$1 AND student.user_account_id IS NOT NULL
             AND student.status IN ('provisional','active')
           GROUP BY 1,2 ORDER BY 1,2"#
    ))
    .bind(tenant)
    .fetch_all(db.pool())
    .await?;
    let mut departments: Vec<String> = cohorts
        .iter()
        .map(|(department, _, _)| department.clone())
        .filter(|value| !value.is_empty())
        .collect();
    departments.dedup();
    let mut years: Vec<String> = cohorts
        .iter()
        .map(|(_, year, _)| year.clone())
        .filter(|value| !value.is_empty())
        .collect();
    years.sort();
    years.dedup();
    Ok(Json(ApiResponse::new(json!({
        "purposes": PURPOSES.iter().map(|(key, label)| json!({"key": key, "label": label})).collect::<Vec<_>>(),
        "manualMethods": MANUAL_METHODS.iter().map(|(key, label)| json!({"key": key, "label": label})).collect::<Vec<_>>(),
        "departments": departments,
        "years": years,
        "studentCount": cohorts.iter().map(|(_, _, count)| count).sum::<i64>(),
        "cohorts": cohorts.iter().map(|(department, year, count)| json!({
            "department": department, "year": year, "count": count
        })).collect::<Vec<_>>(),
        "onlinePaymentsEnabled": crate::razorpay::gateway_configured(),
    }))))
}

const DEPARTMENT_SQL: &str = "COALESCE(department.code, student.department_id, '')";
const YEAR_SQL: &str = "COALESCE(NULLIF(student.profile->>'yearOfStudy',''), NULLIF(student.profile->>'year',''), NULLIF(student.academic_year,''), '')";

#[derive(Debug, Deserialize)]
struct StudentSearch {
    q: Option<String>,
}

async fn search_students(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<StudentSearch>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, MANAGE)?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    let tenant = tenant_id(db.pool(), slug).await?;
    let q = query.q.unwrap_or_default().trim().to_lowercase();
    let rows = sqlx::query(&format!(
        r#"SELECT student.user_account_id::text AS user_id, student.student_number,
                  student.full_name, student.email, {DEPARTMENT_SQL} AS department, {YEAR_SQL} AS year
           FROM core.students student
           LEFT JOIN core.departments department
             ON department.tenant_id=student.tenant_id AND department.id::text=student.department_id
           WHERE student.tenant_id=$1 AND student.user_account_id IS NOT NULL
             AND student.status IN ('provisional','active')
             AND ($2 = '' OR lower(student.full_name) LIKE '%' || $2 || '%'
                  OR lower(student.student_number) LIKE '%' || $2 || '%'
                  OR lower(COALESCE(student.email,'')) LIKE '%' || $2 || '%')
           ORDER BY student.student_number
           LIMIT 50"#
    ))
    .bind(tenant)
    .bind(&q)
    .fetch_all(db.pool())
    .await?;
    let students: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "userId": row.get::<String, _>("user_id"),
                "studentNumber": row.get::<String, _>("student_number"),
                "name": row.get::<String, _>("full_name"),
                "email": row.get::<Option<String>, _>("email"),
                "department": row.get::<String, _>("department"),
                "year": row.get::<String, _>("year"),
            })
        })
        .collect();
    Ok(Json(ApiResponse::new(json!({ "students": students }))))
}

async fn create_request(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<CreateRequestInput>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, MANAGE)?;
    let request = validate_new_request(input, campus_today()).map_err(ApiError::BadRequest)?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;

    let (student_ids, departments, years) = match &request.target {
        Target::All => (None, Vec::new(), Vec::new()),
        Target::Students(ids) => (
            Some(ids.iter().map(|id| id.to_lowercase()).collect::<Vec<_>>()),
            Vec::new(),
            Vec::new(),
        ),
        Target::Cohort { departments, years } => (None, departments.clone(), years.clone()),
    };
    let upper = |values: &[String]| values.iter().map(|v| v.to_uppercase()).collect::<Vec<_>>();
    let targets = sqlx::query(&format!(
        r#"SELECT student.user_account_id::text AS user_id, student.id::text AS student_id,
                  student.student_number, student.full_name,
                  {DEPARTMENT_SQL} AS department, {YEAR_SQL} AS year
           FROM core.students student
           LEFT JOIN core.departments department
             ON department.tenant_id=student.tenant_id AND department.id::text=student.department_id
           WHERE student.tenant_id=$1 AND student.user_account_id IS NOT NULL
             AND student.status IN ('provisional','active')
             AND ($2::text[] IS NULL OR lower(student.user_account_id::text) = ANY($2))
             AND (cardinality($3::text[]) = 0 OR upper({DEPARTMENT_SQL}) = ANY($3))
             AND (cardinality($4::text[]) = 0 OR upper({YEAR_SQL}) = ANY($4))
           ORDER BY student.student_number"#
    ))
    .bind(tenant)
    .bind(&student_ids)
    .bind(upper(&departments))
    .bind(upper(&years))
    .fetch_all(db.pool())
    .await?;
    if targets.is_empty() {
        return Err(ApiError::BadRequest(
            "No active students match this request".into(),
        ));
    }

    let mut tx = db.pool().begin().await?;
    let request_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO campus_ops.payment_requests
             (tenant_id,purpose,title,description,amount,due_date,show_on_dashboard,
              target_mode,target_departments,target_years,created_by,created_by_name)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
           RETURNING id"#,
    )
    .bind(tenant)
    .bind(request.purpose)
    .bind(&request.title)
    .bind(&request.description)
    .bind(request.amount)
    .bind(request.due_date)
    .bind(request.show_on_dashboard)
    .bind(request.target.mode())
    .bind(&departments)
    .bind(&years)
    .bind(&principal.student.id)
    .bind(&principal.student.name)
    .fetch_one(&mut *tx)
    .await?;
    let body = format!(
        "{} · {}{}",
        request.title,
        format_rupees(request.amount),
        request
            .due_date
            .map(|due| format!(" · due {}", due.format("%d %b %Y")))
            .unwrap_or_default()
    );
    for row in &targets {
        let user_id: String = row.get("user_id");
        sqlx::query(
            r#"INSERT INTO campus_ops.payment_request_payers
                 (tenant_id,request_id,user_id,student_id,student_number,student_name,
                  department,year_of_study,amount)
               VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
               ON CONFLICT (tenant_id,request_id,user_id) DO NOTHING"#,
        )
        .bind(tenant)
        .bind(request_id)
        .bind(&user_id)
        .bind(row.get::<String, _>("student_id"))
        .bind(row.get::<String, _>("student_number"))
        .bind(row.get::<String, _>("full_name"))
        .bind(row.get::<String, _>("department"))
        .bind(row.get::<String, _>("year"))
        .bind(request.amount)
        .execute(&mut *tx)
        .await?;
        notify_user(
            &mut tx,
            tenant,
            &user_id,
            "payment_request.created",
            &format!("Payment due: {}", purpose_label(request.purpose)),
            &body,
            json!({"requestId": request_id, "status": "pending"}),
            format!("payment_request:{request_id}:created:{user_id}"),
        )
        .await?;
    }
    record_event(
        &mut tx,
        tenant,
        "payment_request",
        &request_id.to_string(),
        "payment_request.created",
        &principal.student.id,
        &json!({"payers": targets.len(), "amount": request.amount}),
    )
    .await?;
    tx.commit().await?;
    publish(
        &state,
        slug,
        "payment_request.created",
        "payment_request",
        &request_id.to_string(),
    );
    let detail = load_request_detail(&db, tenant, request_id).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(detail))))
}

async fn load_request_detail(
    db: &supercampus_database::Database,
    tenant: Uuid,
    request_id: Uuid,
) -> ApiResult<Value> {
    let row = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM campus_ops.payment_requests request WHERE request.tenant_id=$1 AND request.id=$2"
    ))
    .bind(tenant)
    .bind(request_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("Payment request not found".into()))?;
    let today = campus_today();
    let mut request = request_json(&row, today);
    let due_date: Option<NaiveDate> = row.get("due_date");
    let payers = sqlx::query(
        r#"SELECT id, user_id, student_number, student_name, department, year_of_study,
                  amount::float8 AS amount, status, paid_at, payment_method, payment_reference,
                  razorpay_payment_id, note
           FROM campus_ops.payment_request_payers
           WHERE tenant_id=$1 AND request_id=$2
           ORDER BY CASE status WHEN 'pending' THEN 0 WHEN 'paid' THEN 1 ELSE 2 END,
                    student_number NULLS LAST, student_name"#,
    )
    .bind(tenant)
    .bind(request_id)
    .fetch_all(db.pool())
    .await?;
    request["payers"] = Value::Array(
        payers
            .iter()
            .map(|payer| {
                let status: String = payer.get("status");
                json!({
                    "id": payer.get::<Uuid, _>("id"),
                    "userId": payer.get::<String, _>("user_id"),
                    "studentNumber": payer.get::<Option<String>, _>("student_number"),
                    "name": payer.get::<String, _>("student_name"),
                    "department": payer.get::<Option<String>, _>("department"),
                    "year": payer.get::<Option<String>, _>("year_of_study"),
                    "amount": payer.get::<f64, _>("amount"),
                    "status": display_status(&status, due_date, today),
                    "paidAt": payer.get::<Option<DateTime<Utc>>, _>("paid_at"),
                    "paymentMethod": payer.get::<Option<String>, _>("payment_method"),
                    "paymentReference": payer.get::<Option<String>, _>("payment_reference"),
                    "razorpayPaymentId": payer.get::<Option<String>, _>("razorpay_payment_id"),
                    "note": payer.get::<Option<String>, _>("note"),
                })
            })
            .collect(),
    );
    Ok(request)
}

fn parse_uuid(value: &str, what: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(value.trim()).map_err(|_| ApiError::BadRequest(format!("{what} is invalid")))
}

async fn request_detail(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, &[READ, MANAGE])?;
    let request_id = parse_uuid(&request_id, "Payment request id")?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    let mut detail = load_request_detail(&db, tenant, request_id).await?;
    detail["canManage"] = json!(access.allows(MANAGE));
    Ok(Json(ApiResponse::new(detail)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelInput {
    reason: Option<String>,
}

async fn cancel_request(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<String>,
    Json(input): Json<CancelInput>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, MANAGE)?;
    let request_id = parse_uuid(&request_id, "Payment request id")?;
    let reason = input
        .reason
        .map(|reason| reason.trim().to_owned())
        .filter(|reason| !reason.is_empty());
    if reason
        .as_ref()
        .is_some_and(|reason| reason.chars().count() > 300)
    {
        return Err(ApiError::BadRequest(
            "Reason must be 300 characters or fewer".into(),
        ));
    }
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    let mut tx = db.pool().begin().await?;
    let current = sqlx::query_as::<_, (String, String)>(
        "SELECT status, title FROM campus_ops.payment_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Payment request not found".into()))?;
    if current.0 != "active" {
        return Err(ApiError::Conflict(format!(
            "This request is already {}",
            current.0
        )));
    }
    sqlx::query(
        r#"UPDATE campus_ops.payment_requests
           SET status='cancelled', cancelled_by=$3, cancel_reason=$4, cancelled_at=now(), updated_at=now()
           WHERE tenant_id=$1 AND id=$2"#,
    )
    .bind(tenant)
    .bind(request_id)
    .bind(&principal.student.id)
    .bind(&reason)
    .execute(&mut *tx)
    .await?;
    let cancelled_users: Vec<String> = sqlx::query_scalar(
        r#"UPDATE campus_ops.payment_request_payers
           SET status='cancelled', updated_at=now()
           WHERE tenant_id=$1 AND request_id=$2 AND status='pending'
           RETURNING user_id"#,
    )
    .bind(tenant)
    .bind(request_id)
    .fetch_all(&mut *tx)
    .await?;
    for user_id in &cancelled_users {
        notify_user(
            &mut tx,
            tenant,
            user_id,
            "payment_request.cancelled",
            "Payment request withdrawn",
            &format!("{} no longer needs to be paid.", current.1),
            json!({"requestId": request_id, "status": "cancelled"}),
            format!("payment_request:{request_id}:cancelled:{user_id}"),
        )
        .await?;
    }
    record_event(
        &mut tx,
        tenant,
        "payment_request",
        &request_id.to_string(),
        "payment_request.cancelled",
        &principal.student.id,
        &json!({"reason": reason, "cancelledPayers": cancelled_users.len()}),
    )
    .await?;
    tx.commit().await?;
    publish(
        &state,
        slug,
        "payment_request.cancelled",
        "payment_request",
        &request_id.to_string(),
    );
    let detail = load_request_detail(&db, tenant, request_id).await?;
    Ok(Json(ApiResponse::new(detail)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkPaidInput {
    method: Option<String>,
    reference: Option<String>,
    note: Option<String>,
}

fn validate_mark_paid(
    input: MarkPaidInput,
) -> Result<(&'static str, Option<String>, Option<String>), String> {
    let method = input.method.unwrap_or_default().trim().to_ascii_lowercase();
    let method = MANUAL_METHODS
        .iter()
        .find(|(key, _)| *key == method)
        .map(|(key, _)| *key)
        .ok_or("Choose how the payment was received")?;
    let clean = |value: Option<String>, max: usize, what: &str| -> Result<Option<String>, String> {
        let value = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        if value.as_ref().is_some_and(|v| v.chars().count() > max) {
            return Err(format!("{what} must be {max} characters or fewer"));
        }
        Ok(value)
    };
    Ok((
        method,
        clean(input.reference, 80, "Reference")?,
        clean(input.note, 300, "Note")?,
    ))
}

async fn mark_paid_manually(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path((request_id, payer_id)): Path<(String, String)>,
    Json(input): Json<MarkPaidInput>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, MANAGE)?;
    let request_id = parse_uuid(&request_id, "Payment request id")?;
    let payer_id = parse_uuid(&payer_id, "Payer id")?;
    let (method, reference, note) = validate_mark_paid(input).map_err(ApiError::BadRequest)?;
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    let mut tx = db.pool().begin().await?;
    let payer = sqlx::query_as::<_, (String, String, String, String, f64)>(
        r#"SELECT payer.status, request.status, payer.user_id, request.title, payer.amount::float8
           FROM campus_ops.payment_request_payers payer
           JOIN campus_ops.payment_requests request ON request.id=payer.request_id
           WHERE payer.tenant_id=$1 AND payer.request_id=$2 AND payer.id=$3
           FOR UPDATE OF payer"#,
    )
    .bind(tenant)
    .bind(request_id)
    .bind(payer_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("This student is not part of the request".into()))?;
    let (payer_status, request_status, user_id, title, amount) = payer;
    if payer_status == "paid" {
        return Err(ApiError::Conflict("This student has already paid".into()));
    }
    if payer_status == "cancelled" || request_status == "cancelled" {
        return Err(ApiError::Conflict("This request was cancelled".into()));
    }
    sqlx::query(
        r#"UPDATE campus_ops.payment_request_payers
           SET status='paid', paid_at=now(), payment_method=$3, payment_reference=$4,
               note=$5, marked_by=$6, updated_at=now()
           WHERE tenant_id=$1 AND id=$2"#,
    )
    .bind(tenant)
    .bind(payer_id)
    .bind(method)
    .bind(&reference)
    .bind(&note)
    .bind(&principal.student.id)
    .execute(&mut *tx)
    .await?;
    close_if_settled(&mut tx, tenant, request_id).await?;
    notify_user(
        &mut tx,
        tenant,
        &user_id,
        "payment_request.paid",
        "Payment received",
        &format!(
            "{} · {} marked paid by the accounts office.",
            title,
            format_rupees(amount)
        ),
        json!({"requestId": request_id, "status": "paid"}),
        format!("payment_request:{request_id}:paid:{user_id}"),
    )
    .await?;
    record_event(
        &mut tx,
        tenant,
        "payment_request",
        &request_id.to_string(),
        "payment_request.payer_paid",
        &principal.student.id,
        &json!({"payerId": payer_id, "method": method}),
    )
    .await?;
    tx.commit().await?;
    publish(
        &state,
        slug,
        "payment_request.paid",
        "payment_request",
        &request_id.to_string(),
    );
    let detail = load_request_detail(&db, tenant, request_id).await?;
    Ok(Json(ApiResponse::new(detail)))
}

/// Closes an active request once nobody is left to pay.
async fn close_if_settled(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    request_id: Uuid,
) -> ApiResult<()> {
    sqlx::query(
        r#"UPDATE campus_ops.payment_requests request
           SET status='closed', updated_at=now()
           WHERE request.tenant_id=$1 AND request.id=$2 AND request.status='active'
             AND NOT EXISTS (SELECT 1 FROM campus_ops.payment_request_payers payer
                             WHERE payer.request_id=request.id AND payer.status='pending')"#,
    )
    .bind(tenant)
    .bind(request_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ student

async fn my_requests(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    // Open requests, plus ones paid or withdrawn in the last 30 days so the
    // home card can confirm a payment went through.
    let rows = sqlx::query(
        r#"SELECT payer.id AS payer_id, request.id AS request_id, request.purpose, request.title,
                  request.description, payer.amount::float8 AS amount, request.currency,
                  request.due_date, request.show_on_dashboard, payer.status, payer.paid_at,
                  payer.payment_method, request.created_at
           FROM campus_ops.payment_request_payers payer
           JOIN campus_ops.payment_requests request ON request.id=payer.request_id
           WHERE payer.tenant_id=$1 AND lower(payer.user_id)=lower($2)
             AND (payer.status='pending'
                  OR COALESCE(payer.paid_at, payer.updated_at) > now() - interval '30 days')
           ORDER BY CASE payer.status WHEN 'pending' THEN 0 WHEN 'paid' THEN 1 ELSE 2 END,
                    request.due_date NULLS LAST, request.created_at DESC
           LIMIT 50"#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .fetch_all(db.pool())
    .await?;
    let today = campus_today();
    let requests: Vec<Value> = rows
        .iter()
        .map(|row| {
            let purpose: String = row.get("purpose");
            let status: String = row.get("status");
            let due_date: Option<NaiveDate> = row.get("due_date");
            json!({
                "payerId": row.get::<Uuid, _>("payer_id"),
                "requestId": row.get::<Uuid, _>("request_id"),
                "purpose": purpose,
                "purposeLabel": purpose_label(&purpose),
                "title": row.get::<String, _>("title"),
                "description": row.get::<String, _>("description"),
                "amount": row.get::<f64, _>("amount"),
                "currency": row.get::<String, _>("currency"),
                "dueDate": due_date,
                "showOnDashboard": row.get::<bool, _>("show_on_dashboard"),
                "status": display_status(&status, due_date, today),
                "paidAt": row.get::<Option<DateTime<Utc>>, _>("paid_at"),
                "paymentMethod": row.get::<Option<String>, _>("payment_method"),
                "createdAt": row.get::<DateTime<Utc>, _>("created_at"),
            })
        })
        .collect();
    Ok(Json(ApiResponse::new(json!({
        "requests": requests,
        "onlinePaymentsEnabled": crate::razorpay::gateway_configured(),
    }))))
}

pub(crate) struct Payable {
    pub(crate) payer_id: String,
    pub(crate) amount_paise: i64,
}

/// The open payer row a student is about to pay online. Binds the checkout
/// to the signed-in student's own, still pending row of an active request.
pub(crate) async fn payable_for(
    db: &supercampus_database::Database,
    principal: &AuthPrincipal,
    reference_id: Option<&str>,
) -> ApiResult<Payable> {
    let slug = &principal.student.tenant_id;
    ensure_schema(db.pool(), slug).await;
    let payer_id = parse_uuid(
        reference_id.ok_or_else(|| ApiError::BadRequest("referenceId is required".into()))?,
        "Payment request",
    )?;
    let tenant = tenant_id(db.pool(), slug).await?;
    let (user_id, payer_status, request_status, amount) =
        sqlx::query_as::<_, (String, String, String, f64)>(
            r#"SELECT payer.user_id, payer.status, request.status, payer.amount::float8
               FROM campus_ops.payment_request_payers payer
               JOIN campus_ops.payment_requests request ON request.id=payer.request_id
               WHERE payer.tenant_id=$1 AND payer.id=$2"#,
        )
        .bind(tenant)
        .bind(payer_id)
        .fetch_optional(db.pool())
        .await?
        .ok_or_else(|| ApiError::NotFound("Payment request not found".into()))?;
    if !user_id.eq_ignore_ascii_case(&principal.student.id) {
        return Err(ApiError::NotFound("Payment request not found".into()));
    }
    if payer_status == "paid" {
        return Err(ApiError::Conflict("This request is already paid".into()));
    }
    if payer_status != "pending" || request_status != "active" {
        return Err(ApiError::Conflict(
            "This request is no longer open for payment".into(),
        ));
    }
    Ok(Payable {
        payer_id: payer_id.to_string(),
        amount_paise: to_paise(amount),
    })
}

/// Marks a payer paid after Razorpay confirmed the payment. Idempotent on
/// the payment id. Money that was captured is always recorded, even when the
/// request was withdrawn meanwhile, so the office can refund it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn mark_paid_online(
    state: &AppState,
    tenant_slug: &str,
    user_id: &str,
    payer_id: &str,
    order_id: &str,
    payment_id: &str,
    amount_paise: i64,
) -> ApiResult<Value> {
    let db = state.tenant_database(tenant_slug).await?;
    ensure_schema(db.pool(), tenant_slug).await;
    let tenant = tenant_id(db.pool(), tenant_slug).await?;
    let payer_id = parse_uuid(payer_id, "Payment request")?;
    let mut tx = db.pool().begin().await?;
    let (request_id, owner, status, amount, existing_payment, title) =
        sqlx::query_as::<_, (Uuid, String, String, f64, Option<String>, String)>(
            r#"SELECT payer.request_id, payer.user_id, payer.status, payer.amount::float8,
                      payer.razorpay_payment_id, request.title
               FROM campus_ops.payment_request_payers payer
               JOIN campus_ops.payment_requests request ON request.id=payer.request_id
               WHERE payer.tenant_id=$1 AND payer.id=$2
               FOR UPDATE OF payer"#,
        )
        .bind(tenant)
        .bind(payer_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::NotFound("Payment request not found".into()))?;
    if !owner.eq_ignore_ascii_case(user_id) {
        return Err(ApiError::BadRequest(
            "This payment order belongs to a different account".into(),
        ));
    }
    if to_paise(amount) != amount_paise {
        return Err(ApiError::BadRequest(
            "The paid amount does not match the request".into(),
        ));
    }
    let result = json!({"payerId": payer_id, "requestId": request_id, "status": "paid"});
    if status == "paid" {
        if existing_payment.as_deref() != Some(payment_id) {
            tracing::warn!(
                %payer_id, payment_id,
                "online payment captured for a request already marked paid; refund may be due"
            );
        }
        tx.rollback().await?;
        return Ok(result);
    }
    sqlx::query(
        r#"UPDATE campus_ops.payment_request_payers
           SET status='paid', paid_at=now(), payment_method='razorpay', payment_reference=$3,
               razorpay_order_id=$4, razorpay_payment_id=$3, updated_at=now()
           WHERE tenant_id=$1 AND id=$2"#,
    )
    .bind(tenant)
    .bind(payer_id)
    .bind(payment_id)
    .bind(order_id)
    .execute(&mut *tx)
    .await?;
    close_if_settled(&mut tx, tenant, request_id).await?;
    notify_user(
        &mut tx,
        tenant,
        &owner,
        "payment_request.paid",
        "Payment received",
        &format!("{} · {} paid online.", title, format_rupees(amount)),
        json!({"requestId": request_id, "status": "paid"}),
        format!("payment_request:{request_id}:paid:{owner}"),
    )
    .await?;
    record_event(
        &mut tx,
        tenant,
        "payment_request",
        &request_id.to_string(),
        "payment_request.payer_paid",
        &owner,
        &json!({"payerId": payer_id, "method": "razorpay", "paymentId": payment_id}),
    )
    .await?;
    tx.commit().await?;
    publish(
        state,
        tenant_slug,
        "payment_request.paid",
        "payment_request",
        &request_id.to_string(),
    );
    Ok(result)
}

// ------------------------------------------------------------------ order ledger

/// Records a freshly created Razorpay order.
pub(crate) async fn record_order_created(
    db: &supercampus_database::Database,
    principal: &AuthPrincipal,
    order: &RazorpayOrder,
    purpose: &str,
    reference_id: Option<&str>,
    shop_key: Option<&str>,
) -> ApiResult<()> {
    let slug = &principal.student.tenant_id;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    sqlx::query(
        r#"INSERT INTO campus_ops.razorpay_orders
             (tenant_id,order_id,user_id,user_name,user_email,user_number,purpose,reference_id,
              shop_key,receipt,amount_paise,currency)
           VALUES($1,$2,$3,$4,$5,NULLIF($6,''),$7,$8,$9,$10,$11,$12)
           ON CONFLICT (tenant_id,order_id) DO NOTHING"#,
    )
    .bind(tenant)
    .bind(&order.id)
    .bind(&principal.student.id)
    .bind(&principal.student.name)
    .bind(&principal.student.email)
    .bind(&principal.student.roll)
    .bind(purpose)
    .bind(reference_id)
    .bind(shop_key)
    .bind(&order.receipt)
    .bind(order.amount)
    .bind(&order.currency)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Marks an order fulfilled after checkout verification (or a recovery).
pub(crate) async fn record_order_fulfilled(
    state: &AppState,
    tenant_slug: &str,
    order: &RazorpayOrder,
    payment_id: &str,
    recovered: bool,
) -> ApiResult<()> {
    let db = state.tenant_database(tenant_slug).await?;
    ensure_schema(db.pool(), tenant_slug).await;
    let tenant = tenant_id(db.pool(), tenant_slug).await?;
    // Razorpay reports an order "paid" once one of its payments is captured.
    let captured = order.status.as_deref() == Some("paid");
    sqlx::query(
        r#"UPDATE campus_ops.razorpay_orders
           SET payment_id=$3,
               status=CASE WHEN $4 THEN 'captured'
                           WHEN status IN ('created','failed') THEN 'authorized'
                           ELSE status END,
               captured_at=CASE WHEN $4 THEN COALESCE(captured_at, now()) ELSE captured_at END,
               fulfilled=true, fulfilled_at=COALESCE(fulfilled_at, now()),
               recovered=recovered OR $5, updated_at=now()
           WHERE tenant_id=$1 AND order_id=$2"#,
    )
    .bind(tenant)
    .bind(&order.id)
    .bind(payment_id)
    .bind(captured)
    .bind(recovered)
    .execute(db.pool())
    .await?;
    publish(
        state,
        tenant_slug,
        "online_payment.updated",
        "online_payment",
        &order.id,
    );
    Ok(())
}

// ------------------------------------------------------------------ online payments

#[derive(Debug, Deserialize)]
struct OnlinePaymentsQuery {
    from: Option<String>,
    to: Option<String>,
    status: Option<String>,
    q: Option<String>,
}

fn parse_range(
    from: Option<&str>,
    to: Option<&str>,
    today: NaiveDate,
) -> ApiResult<(NaiveDate, NaiveDate)> {
    let parse = |value: Option<&str>, fallback: NaiveDate| -> ApiResult<NaiveDate> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(fallback),
            Some(value) => NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| ApiError::BadRequest("Dates must be YYYY-MM-DD".into())),
        }
    };
    let to = parse(to, today)?;
    let from = parse(from, to - Duration::days(29))?;
    if from > to {
        return Err(ApiError::BadRequest(
            "From date must be before To date".into(),
        ));
    }
    if (to - from).num_days() > 366 {
        return Err(ApiError::BadRequest(
            "Choose a range of one year or less".into(),
        ));
    }
    Ok((from, to))
}

/// The tracking state of one payment: pending, credited, captured but not
/// credited (the recovery bucket), failed or refunded.
fn tracking_state(status: &str, fulfilled: bool) -> &'static str {
    match status {
        "failed" => "failed",
        "refunded" => "refunded",
        _ if fulfilled => "credited",
        "captured" => "captured_not_credited",
        _ => "pending",
    }
}

fn purpose_title(purpose: &str) -> &'static str {
    match purpose {
        "wallet_top_up" => "Wallet top-up",
        "tuition_fee" => "Tuition fee",
        "payment_request" => "Payment request",
        "guardian_fee_link" => "Fee payment link",
        _ => "Payment",
    }
}

const ONLINE_PAYMENTS_SQL: &str = r#"
WITH combined AS (
    SELECT o.order_id, o.payment_id, o.user_id, o.user_name, o.user_email, o.user_number,
           o.purpose, o.reference_id, o.amount_paise, o.currency, o.status, o.payment_method,
           o.error_code, o.error_description, o.captured_at, o.fulfilled, o.fulfilled_at,
           o.recovered, o.fee_paise, o.tax_paise, o.settlement_id, o.settled, o.settled_at,
           o.last_synced_at, o.created_at, 'checkout'::text AS source,
           COALESCE(request.title, shop.name) AS detail
    FROM campus_ops.razorpay_orders o
    LEFT JOIN campus_ops.payment_request_payers payer
      ON o.purpose='payment_request' AND payer.id::text=o.reference_id
    LEFT JOIN campus_ops.payment_requests request ON request.id=payer.request_id
    LEFT JOIN campus_ops.shops shop ON shop.tenant_id=o.tenant_id AND shop.shop_key=o.shop_key
    WHERE o.tenant_id=$1
    UNION ALL
    SELECT t.reference_id, substr(t.idempotency_key, 10), t.user_id, s.full_name, s.email,
           s.student_number, 'wallet_top_up', NULL, round(t.amount*100)::bigint, 'INR',
           'captured', NULL, NULL, NULL, t.created_at, true, t.created_at, false, NULL, NULL,
           NULL, NULL, NULL, NULL, t.created_at, 'wallet_ledger', shop.name
    FROM campus_ops.canteen_wallet_transactions t
    LEFT JOIN core.students s
      ON s.tenant_id=t.tenant_id AND lower(s.user_account_id::text)=lower(t.user_id)
    LEFT JOIN campus_ops.shops shop ON shop.tenant_id=t.tenant_id AND shop.shop_key=t.shop_key
    WHERE t.tenant_id=$1 AND t.transaction_type='online_top_up' AND t.reference_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM campus_ops.razorpay_orders o
                      WHERE o.tenant_id=t.tenant_id AND o.order_id=t.reference_id)
    UNION ALL
    SELECT r.data->>'razorpayOrderId', r.data->>'paymentReference', r.data->>'studentId',
           s.full_name, r.data->>'studentEmail', r.data->>'studentNumber', 'tuition_fee', NULL,
           CASE WHEN (r.data->>'amountPaise') ~ '^[0-9]+$' THEN (r.data->>'amountPaise')::bigint
                WHEN (r.data->>'amount') ~ '^[0-9]+(\.[0-9]+)?$'
                  THEN round((r.data->>'amount')::numeric*100)::bigint
                ELSE 0 END,
           COALESCE(r.data->>'currency','INR'), 'captured', NULL, NULL, NULL, r.created_at, true,
           r.created_at, false, NULL, NULL, NULL, NULL, NULL, NULL, r.created_at, 'fee_records',
           NULL
    FROM platform.dynamic_records r
    LEFT JOIN core.students s
      ON s.tenant_id=r.tenant_id AND lower(s.user_account_id::text)=lower(r.data->>'studentId')
    WHERE r.tenant_id=$1 AND r.module_key='fees' AND r.record_type='payments'
      AND r.data->>'method'='Razorpay' AND COALESCE(r.data->>'razorpayOrderId','')<>''
      AND NOT EXISTS (SELECT 1 FROM campus_ops.razorpay_orders o
                      WHERE o.tenant_id=r.tenant_id AND o.order_id=r.data->>'razorpayOrderId')
    UNION ALL
    SELECT l.provider_link_id, l.payment_id, l.student_user_id, s.full_name, l.student_email,
           l.student_number, 'guardian_fee_link', l.fee_record_id::text, l.amount_paise,
           l.currency, CASE WHEN l.status='paid' THEN 'captured' ELSE 'created' END, NULL, NULL,
           NULL, l.paid_at, l.status='paid', l.paid_at, false, NULL, NULL, NULL, NULL, NULL,
           NULL, l.created_at, 'payment_link', l.guardian_name
    FROM campus_ops.guardian_fee_payment_links l
    LEFT JOIN core.students s ON s.tenant_id=l.tenant_id AND s.id=l.student_id
    WHERE l.tenant_id=$1
)
SELECT * FROM combined
WHERE created_at >= $2 AND created_at < $3
  AND ($4 = '' OR lower(COALESCE(order_id,'')) LIKE '%' || $4 || '%'
       OR lower(COALESCE(payment_id,'')) LIKE '%' || $4 || '%'
       OR lower(COALESCE(user_name,'')) LIKE '%' || $4 || '%'
       OR lower(COALESCE(user_email,'')) LIKE '%' || $4 || '%'
       OR lower(COALESCE(user_number,'')) LIKE '%' || $4 || '%')
ORDER BY created_at DESC
LIMIT 5000
"#;

fn ist_midnight(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight")) - Duration::minutes(330)
}

async fn online_payments(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<OnlinePaymentsQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, &[ONLINE_READ, ONLINE_RECONCILE])?;
    let (from, to) = parse_range(query.from.as_deref(), query.to.as_deref(), campus_today())?;
    let status_filter = match query.status.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some(
            value @ ("pending"
            | "credited"
            | "captured_not_credited"
            | "failed"
            | "refunded"
            | "recovered"),
        ) => Some(value.to_owned()),
        Some(_) => return Err(ApiError::BadRequest("Unknown status filter".into())),
    };
    let search = query.q.unwrap_or_default().trim().to_lowercase();
    let slug = &principal.student.tenant_id;
    let db = state.tenant_database(slug).await?;
    ensure_schema(db.pool(), slug).await;
    let tenant = tenant_id(db.pool(), slug).await?;
    let rows = sqlx::query(ONLINE_PAYMENTS_SQL)
        .bind(tenant)
        .bind(ist_midnight(from))
        .bind(ist_midnight(to + Duration::days(1)))
        .bind(&search)
        .fetch_all(db.pool())
        .await?;

    let mut summary = OnlineSummary::default();
    let mut items = Vec::new();
    for row in &rows {
        let status: String = row.get("status");
        let fulfilled: bool = row.get("fulfilled");
        let recovered: bool = row.get("recovered");
        let amount: i64 = row.get("amount_paise");
        let settled: Option<bool> = row.get("settled");
        let state_key = tracking_state(&status, fulfilled);
        summary.add(state_key, amount, recovered, settled);
        let include = match status_filter.as_deref() {
            None => true,
            Some("recovered") => recovered,
            Some(filter) => filter == state_key,
        };
        if !include || items.len() >= 300 {
            continue;
        }
        let purpose: String = row.get("purpose");
        items.push(json!({
            "orderId": row.get::<Option<String>, _>("order_id"),
            "paymentId": row.get::<Option<String>, _>("payment_id"),
            "source": row.get::<String, _>("source"),
            "purpose": purpose,
            "purposeLabel": purpose_title(&purpose),
            "detail": row.get::<Option<String>, _>("detail"),
            "referenceId": row.get::<Option<String>, _>("reference_id"),
            "userId": row.get::<Option<String>, _>("user_id"),
            "userName": row.get::<Option<String>, _>("user_name"),
            "userEmail": row.get::<Option<String>, _>("user_email"),
            "userNumber": row.get::<Option<String>, _>("user_number"),
            "amountPaise": amount,
            "amount": amount as f64 / 100.0,
            "currency": row.get::<String, _>("currency"),
            "gatewayStatus": status,
            "state": state_key,
            "paymentMethod": row.get::<Option<String>, _>("payment_method"),
            "errorCode": row.get::<Option<String>, _>("error_code"),
            "errorDescription": row.get::<Option<String>, _>("error_description"),
            "capturedAt": row.get::<Option<DateTime<Utc>>, _>("captured_at"),
            "fulfilled": fulfilled,
            "fulfilledAt": row.get::<Option<DateTime<Utc>>, _>("fulfilled_at"),
            "recovered": recovered,
            "feePaise": row.get::<Option<i64>, _>("fee_paise"),
            "taxPaise": row.get::<Option<i64>, _>("tax_paise"),
            "settlementId": row.get::<Option<String>, _>("settlement_id"),
            "settled": settled,
            "settledAt": row.get::<Option<DateTime<Utc>>, _>("settled_at"),
            "lastSyncedAt": row.get::<Option<DateTime<Utc>>, _>("last_synced_at"),
            "createdAt": row.get::<DateTime<Utc>, _>("created_at"),
        }));
    }
    let last_synced: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT max(last_synced_at) FROM campus_ops.razorpay_orders WHERE tenant_id=$1",
    )
    .bind(tenant)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({
        "from": from,
        "to": to,
        "summary": summary.to_json(),
        "payments": items,
        "truncated": rows.len() >= 5000,
        "gatewayConfigured": crate::razorpay::gateway_configured(),
        "canReconcile": access.allows(ONLINE_RECONCILE),
        "lastSyncedAt": last_synced,
    }))))
}

#[derive(Debug, Default, PartialEq)]
struct OnlineSummary {
    total: i64,
    credited_count: i64,
    credited_paise: i64,
    captured_not_credited_count: i64,
    captured_not_credited_paise: i64,
    pending_count: i64,
    failed_count: i64,
    refunded_count: i64,
    recovered_count: i64,
    settlement_known: i64,
    settled_count: i64,
    settled_paise: i64,
}

impl OnlineSummary {
    fn add(&mut self, state: &str, amount: i64, recovered: bool, settled: Option<bool>) {
        self.total += 1;
        match state {
            "credited" => {
                self.credited_count += 1;
                self.credited_paise += amount;
            }
            "captured_not_credited" => {
                self.captured_not_credited_count += 1;
                self.captured_not_credited_paise += amount;
            }
            "failed" => self.failed_count += 1,
            "refunded" => self.refunded_count += 1,
            _ => self.pending_count += 1,
        }
        if recovered {
            self.recovered_count += 1;
        }
        if let Some(settled) = settled {
            self.settlement_known += 1;
            if settled {
                self.settled_count += 1;
                self.settled_paise += amount;
            }
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "totalRecords": self.total,
            "creditedCount": self.credited_count,
            "creditedAmount": self.credited_paise as f64 / 100.0,
            "capturedNotCreditedCount": self.captured_not_credited_count,
            "capturedNotCreditedAmount": self.captured_not_credited_paise as f64 / 100.0,
            "pendingCount": self.pending_count,
            "failedCount": self.failed_count,
            "refundedCount": self.refunded_count,
            "recoveredCount": self.recovered_count,
            "settlementKnownCount": self.settlement_known,
            "settledCount": self.settled_count,
            "settledAmount": self.settled_paise as f64 / 100.0,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SyncInput {
    from: Option<String>,
    to: Option<String>,
}

/// The attempt that decides an order's state: a captured payment wins, then
/// a refunded, an authorized, and finally the newest failed attempt.
fn decisive_payment(items: &[Value]) -> Option<&Value> {
    let rank = |item: &Value| match item.get("status").and_then(Value::as_str) {
        Some("captured") => 4,
        Some("refunded") => 3,
        Some("authorized") => 2,
        Some("failed") => 1,
        _ => 0,
    };
    items.iter().max_by(|left, right| {
        rank(left).cmp(&rank(right)).then_with(|| {
            let created =
                |item: &Value| item.get("created_at").and_then(Value::as_i64).unwrap_or(0);
            created(left).cmp(&created(right))
        })
    })
}

fn gateway_status(payment_status: &str) -> Option<&'static str> {
    match payment_status {
        "captured" => Some("captured"),
        "authorized" => Some("authorized"),
        "failed" => Some("failed"),
        "refunded" => Some("refunded"),
        _ => None,
    }
}

/// Days whose settlement reconciliation a sync reads: the range plus three
/// days (T+2 settlement), never beyond today, at most 35 days.
fn settlement_days(from: NaiveDate, to: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    let last = (to + Duration::days(3)).min(today);
    let mut days = Vec::new();
    let mut day = from;
    while day <= last && days.len() < 35 {
        days.push(day);
        day += Duration::days(1);
    }
    days
}

fn unix_time(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
}

async fn sync_online_payments(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<SyncInput>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, ONLINE_RECONCILE)?;
    if !crate::razorpay::gateway_configured() {
        return Err(ApiError::ServiceUnavailable(
            "Razorpay is not configured".into(),
        ));
    }
    let today = campus_today();
    let (from, to) = parse_range(input.from.as_deref(), input.to.as_deref(), today)?;
    let slug = principal.student.tenant_id.clone();
    let db = state.tenant_database(&slug).await?;
    ensure_schema(db.pool(), &slug).await;
    let tenant = tenant_id(db.pool(), &slug).await?;

    let candidates =
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String, bool)>(
            r#"SELECT order_id, user_id, user_email, user_number, purpose, fulfilled
           FROM campus_ops.razorpay_orders
           WHERE tenant_id=$1 AND created_at >= $2 AND created_at < $3
             AND (status IN ('created','authorized') OR (status='captured' AND NOT fulfilled))
           ORDER BY created_at
           LIMIT 100"#,
        )
        .bind(tenant)
        .bind(ist_midnight(from))
        .bind(ist_midnight(to + Duration::days(1)))
        .fetch_all(db.pool())
        .await?;

    let mut checked = 0;
    let mut captured = 0;
    let mut recovered = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = Vec::new();
    for (order_id, user_id, user_email, user_number, purpose, fulfilled) in candidates {
        checked += 1;
        let payments = match crate::razorpay::fetch_order_payments(&order_id).await {
            Ok(payments) => payments,
            Err(error) => {
                errors.push(format!("{order_id}: {}", provider_message(&error)));
                continue;
            }
        };
        let Some(payment) = decisive_payment(&payments) else {
            sqlx::query("UPDATE campus_ops.razorpay_orders SET last_synced_at=now() WHERE tenant_id=$1 AND order_id=$2")
                .bind(tenant)
                .bind(&order_id)
                .execute(db.pool())
                .await?;
            continue;
        };
        let payment_status = payment.get("status").and_then(Value::as_str).unwrap_or("");
        let Some(status) = gateway_status(payment_status) else {
            continue;
        };
        let payment_id = payment.get("id").and_then(Value::as_str).map(str::to_owned);
        sqlx::query(
            r#"UPDATE campus_ops.razorpay_orders
               SET status=$3, payment_id=COALESCE($4, payment_id), payment_method=$5,
                   error_code=$6, error_description=$7,
                   captured_at=CASE WHEN $3 IN ('captured','refunded')
                                    THEN COALESCE(captured_at, $8, now()) ELSE captured_at END,
                   fee_paise=COALESCE($9, fee_paise), tax_paise=COALESCE($10, tax_paise),
                   last_synced_at=now(), updated_at=now()
               WHERE tenant_id=$1 AND order_id=$2"#,
        )
        .bind(tenant)
        .bind(&order_id)
        .bind(status)
        .bind(&payment_id)
        .bind(payment.get("method").and_then(Value::as_str))
        .bind(payment.get("error_code").and_then(Value::as_str))
        .bind(payment.get("error_description").and_then(Value::as_str))
        .bind(unix_time(payment.get("created_at")))
        .bind(payment.get("fee").and_then(Value::as_i64))
        .bind(payment.get("tax").and_then(Value::as_i64))
        .execute(db.pool())
        .await?;
        match status {
            "failed" => failed += 1,
            "captured" => captured += 1,
            _ => {}
        }
        if status != "captured" || fulfilled {
            continue;
        }
        let Some(payment_id) = payment_id else {
            continue;
        };
        // Captured but never credited: apply the campus side now.
        let recovery = async {
            let order = crate::razorpay::fetch_order(&order_id).await?;
            let belongs = order.notes.get("tenantId").and_then(Value::as_str)
                == Some(slug.as_str())
                && order
                    .notes
                    .get("studentId")
                    .and_then(Value::as_str)
                    .is_some_and(|student| student.eq_ignore_ascii_case(&user_id));
            if !belongs {
                return Err(ApiError::BadRequest(
                    "order notes do not match the ledger".into(),
                ));
            }
            let purpose = crate::razorpay::payment_purpose(Some(&purpose))?;
            let owner = PaymentOwner {
                tenant_slug: slug.clone(),
                user_id: user_id.clone(),
                roll: user_number.clone().unwrap_or_default(),
                email: user_email.clone().unwrap_or_default(),
            };
            crate::razorpay::fulfil_order(&state, &owner, &order, &order_id, &payment_id, purpose)
                .await?;
            record_order_fulfilled(&state, &slug, &order, &payment_id, true).await
        };
        match recovery.await {
            Ok(()) => recovered += 1,
            Err(error) => errors.push(format!("{order_id}: {}", provider_message(&error))),
        }
    }

    // Settlements come only from Razorpay's reconciliation report.
    let mut settlements_matched = std::collections::HashSet::new();
    let mut settlement_error = None;
    'days: for day in settlement_days(from, to, today) {
        let mut skip = 0;
        loop {
            let items = match crate::razorpay::fetch_settlement_recon(
                day.year(),
                day.month(),
                day.day(),
                skip,
            )
            .await
            {
                Ok(items) => items,
                Err(error) => {
                    settlement_error = Some(provider_message(&error));
                    break 'days;
                }
            };
            for item in &items {
                let Some(entity) = item.get("entity_id").and_then(Value::as_str) else {
                    continue;
                };
                if !entity.starts_with("pay_") {
                    continue;
                }
                let updated = sqlx::query(
                    r#"UPDATE campus_ops.razorpay_orders
                       SET settlement_id=COALESCE($3, settlement_id), settled=$4,
                           settled_at=COALESCE($5, settled_at),
                           fee_paise=COALESCE($6, fee_paise), tax_paise=COALESCE($7, tax_paise),
                           last_synced_at=now(), updated_at=now()
                       WHERE tenant_id=$1 AND payment_id=$2"#,
                )
                .bind(tenant)
                .bind(entity)
                .bind(item.get("settlement_id").and_then(Value::as_str))
                .bind(item.get("settled").and_then(Value::as_bool).unwrap_or(true))
                .bind(unix_time(item.get("settled_at")))
                .bind(item.get("fee").and_then(Value::as_i64))
                .bind(item.get("tax").and_then(Value::as_i64))
                .execute(db.pool())
                .await?;
                if updated.rows_affected() > 0 {
                    settlements_matched.insert(entity.to_owned());
                }
            }
            if items.len() < 1000 {
                break;
            }
            skip += items.len();
        }
    }
    publish(
        &state,
        &slug,
        "online_payment.synced",
        "online_payment",
        "sync",
    );
    Ok(Json(ApiResponse::new(json!({
        "checked": checked,
        "captured": captured,
        "recovered": recovered,
        "failed": failed,
        "settlementsMatched": settlements_matched.len(),
        "settlementError": settlement_error,
        "errors": errors,
        "syncedAt": Utc::now(),
    }))))
}

fn provider_message(error: &ApiError) -> String {
    match error {
        ApiError::PaymentProvider(message)
        | ApiError::PaymentProviderUnauthorized(message)
        | ApiError::BadRequest(message)
        | ApiError::Conflict(message)
        | ApiError::NotFound(message)
        | ApiError::ServiceUnavailable(message) => message.clone(),
        _ => "unexpected error".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn input() -> CreateRequestInput {
        CreateRequestInput {
            purpose: Some("fine".into()),
            title: Some("Library fine".into()),
            description: Some("Late return of two books".into()),
            amount: Some(150.456),
            due_date: Some("2026-10-10".into()),
            show_on_dashboard: None,
            target: Some("all".into()),
            student_user_ids: vec![],
            departments: vec![],
            years: vec![],
        }
    }

    #[test]
    fn validates_a_complete_request_and_rounds_the_amount() {
        let request = validate_new_request(input(), date("2026-09-29")).unwrap();
        assert_eq!(request.purpose, "fine");
        assert_eq!(request.amount, 150.46);
        assert_eq!(request.due_date, Some(date("2026-10-10")));
        assert!(request.show_on_dashboard);
        assert_eq!(request.target, Target::All);
    }

    #[test]
    fn rejects_short_descriptions_bad_amounts_and_past_due_dates() {
        let today = date("2026-09-29");
        let mut short = input();
        short.description = Some("too short".into());
        assert!(validate_new_request(short, today).is_err());
        let mut zero = input();
        zero.amount = Some(0.4);
        assert!(validate_new_request(zero, today).is_err());
        let mut huge = input();
        huge.amount = Some(2_000_000.0);
        assert!(validate_new_request(huge, today).is_err());
        let mut past = input();
        past.due_date = Some("2026-09-28".into());
        assert!(validate_new_request(past, today).is_err());
        let mut purpose = input();
        purpose.purpose = Some("bribe".into());
        assert!(validate_new_request(purpose, today).is_err());
    }

    #[test]
    fn targets_need_students_or_a_cohort() {
        let today = date("2026-09-29");
        let mut students = input();
        students.target = Some("students".into());
        assert!(validate_new_request(students, today).is_err());
        let mut chosen = input();
        chosen.target = Some("students".into());
        chosen.student_user_ids = vec![" b ".into(), "a".into(), "b".into(), "".into()];
        assert_eq!(
            validate_new_request(chosen, today).unwrap().target,
            Target::Students(vec!["a".into(), "b".into()])
        );
        let mut cohort = input();
        cohort.target = Some("cohort".into());
        assert!(validate_new_request(cohort, today).is_err());
        let mut year = input();
        year.target = Some("cohort".into());
        year.years = vec!["I".into()];
        assert_eq!(
            validate_new_request(year, today).unwrap().target,
            Target::Cohort {
                departments: vec![],
                years: vec!["I".into()]
            }
        );
    }

    #[test]
    fn payer_status_turns_overdue_only_while_pending() {
        let today = date("2026-09-29");
        assert_eq!(
            display_status("pending", Some(date("2026-09-28")), today),
            "overdue"
        );
        assert_eq!(display_status("pending", Some(today), today), "pending");
        assert_eq!(display_status("pending", None, today), "pending");
        assert_eq!(
            display_status("paid", Some(date("2026-01-01")), today),
            "paid"
        );
        assert_eq!(
            display_status("cancelled", Some(date("2026-01-01")), today),
            "cancelled"
        );
    }

    #[test]
    fn manual_payments_need_a_known_method() {
        let ok = validate_mark_paid(MarkPaidInput {
            method: Some("Cash".into()),
            reference: Some("  ".into()),
            note: Some("Paid at counter".into()),
        })
        .unwrap();
        assert_eq!(ok, ("cash", None, Some("Paid at counter".into())));
        assert!(
            validate_mark_paid(MarkPaidInput {
                method: Some("razorpay".into()),
                reference: None,
                note: None
            })
            .is_err()
        );
    }

    #[test]
    fn tracking_state_separates_the_recovery_bucket() {
        assert_eq!(tracking_state("captured", true), "credited");
        assert_eq!(tracking_state("captured", false), "captured_not_credited");
        assert_eq!(tracking_state("created", false), "pending");
        assert_eq!(tracking_state("authorized", false), "pending");
        assert_eq!(tracking_state("failed", false), "failed");
        assert_eq!(tracking_state("refunded", true), "refunded");
    }

    #[test]
    fn summary_counts_amounts_by_state_and_settlement() {
        let mut summary = OnlineSummary::default();
        summary.add("credited", 10_000, false, Some(true));
        summary.add("credited", 5_000, true, Some(false));
        summary.add("captured_not_credited", 2_500, false, None);
        summary.add("pending", 900, false, None);
        summary.add("failed", 700, false, None);
        let json = summary.to_json();
        assert_eq!(json["totalRecords"], 5);
        assert_eq!(json["creditedAmount"], 150.0);
        assert_eq!(json["capturedNotCreditedAmount"], 25.0);
        assert_eq!(json["pendingCount"], 1);
        assert_eq!(json["failedCount"], 1);
        assert_eq!(json["recoveredCount"], 1);
        assert_eq!(json["settlementKnownCount"], 2);
        assert_eq!(json["settledAmount"], 100.0);
    }

    #[test]
    fn a_captured_attempt_decides_the_order() {
        let items = vec![
            json!({"id": "pay_1", "status": "failed", "created_at": 30}),
            json!({"id": "pay_2", "status": "captured", "created_at": 10}),
            json!({"id": "pay_3", "status": "failed", "created_at": 40}),
        ];
        assert_eq!(decisive_payment(&items).unwrap()["id"], "pay_2");
        let failures = vec![
            json!({"id": "pay_1", "status": "failed", "created_at": 30}),
            json!({"id": "pay_3", "status": "failed", "created_at": 40}),
        ];
        assert_eq!(decisive_payment(&failures).unwrap()["id"], "pay_3");
        assert!(decisive_payment(&[]).is_none());
    }

    #[test]
    fn settlement_days_cover_t_plus_two_without_passing_today() {
        let days = settlement_days(date("2026-09-20"), date("2026-09-25"), date("2026-09-27"));
        assert_eq!(days.first(), Some(&date("2026-09-20")));
        assert_eq!(days.last(), Some(&date("2026-09-27")));
        let long = settlement_days(date("2026-01-01"), date("2026-06-01"), date("2026-09-27"));
        assert_eq!(long.len(), 35);
    }

    #[test]
    fn ranges_default_to_thirty_days_and_reject_inverted_dates() {
        let today = date("2026-09-29");
        let (from, to) = parse_range(None, None, today).unwrap();
        assert_eq!(to, today);
        assert_eq!(from, date("2026-08-31"));
        assert!(parse_range(Some("2026-09-30"), Some("2026-09-01"), today).is_err());
        assert!(parse_range(Some("bad"), None, today).is_err());
    }

    #[test]
    fn purposes_have_labels() {
        assert_eq!(purpose_label("electricity"), "Electricity bill");
        assert_eq!(purpose_label("unknown"), "Other");
        assert_eq!(purpose_key(Some(" FINE ")), Some("fine"));
        assert_eq!(purpose_key(None), Some("other"));
    }
}
