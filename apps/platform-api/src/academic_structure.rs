//! The institution's academic structure: departments, programmes, classes
//! (sections of a yearly batch) and subjects.
//!
//! These rows already live in the canonical `core` tables that Student
//! Master, attendance, timetable, marks, push broadcasts and payment requests
//! read. This module is the one place an administrator edits them, and its
//! catalog is the one lookup every picker in the apps reads, so a programme
//! added here shows up everywhere without a second list to keep in sync.
//!
//! Writes are gated by the `academics.programme.*` and `academics.subject.*`
//! grants at institution scope. Nothing here branches on a role name.
//! Deactivating is the only removal: history (attendance, marks, enrolments)
//! keeps pointing at the rows, and a row still carrying active students
//! cannot be deactivated.

use anyhow::Context;
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub const PROGRAMME_READ: &str = "academics.programme.read";
pub const PROGRAMME_CREATE: &str = "academics.programme.create";
pub const PROGRAMME_UPDATE: &str = "academics.programme.update";
pub const SUBJECT_READ: &str = "academics.subject.read";
pub const SUBJECT_CREATE: &str = "academics.subject.create";
pub const SUBJECT_UPDATE: &str = "academics.subject.update";

/// Grants whose screens pick a department, programme, class or subject. The
/// catalog is names and codes only, so anyone who works with students or
/// classes may read it; the edit grants are checked separately.
const LOOKUP_GRANTS: &[&str] = &[
    PROGRAMME_READ,
    PROGRAMME_CREATE,
    PROGRAMME_UPDATE,
    SUBJECT_READ,
    SUBJECT_CREATE,
    SUBJECT_UPDATE,
    "academics.assignments.read",
    "academics.assignments.manage",
    "academics.timetable.read",
    "academics.timetable.manage",
    "academics.attendance.read",
    "academics.marks.read",
    "attendance.roster.read",
    "attendance.records.read",
    "students.directory.read",
    "students.directory.update",
    "notifications.broadcast.read",
    "notifications.broadcast.send",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/catalog", get(catalog))
        .route("/departments", post(create_department))
        .route("/departments/{department_id}", put(update_department))
        .route("/programmes", post(create_programme))
        .route("/programmes/{programme_id}", put(update_programme))
        .route("/classes", post(create_class))
        .route("/classes/{class_id}", put(update_class))
        .route("/subjects", post(create_subject))
        .route("/subjects/{subject_id}", put(update_subject))
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogQuery {
    #[serde(default)]
    include_inactive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DepartmentRequest {
    code: String,
    name: String,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProgrammeRequest {
    department_id: Uuid,
    code: String,
    name: String,
    #[serde(default)]
    duration_terms: Option<i32>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateClassRequest {
    programme_id: Uuid,
    year_of_study: i32,
    section_code: String,
    #[serde(default)]
    capacity: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateClassRequest {
    section_code: String,
    #[serde(default)]
    capacity: Option<i32>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubjectRequest {
    department_id: Uuid,
    code: String,
    name: String,
    #[serde(default)]
    credits: Option<f64>,
    #[serde(default)]
    active: Option<bool>,
}

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

fn may_read(access: &EffectiveAccess) -> bool {
    LOOKUP_GRANTS
        .iter()
        .any(|permission| access.allows(permission))
}

/// A structural edit changes what the whole institution sees, so it needs the
/// grant at institution (or wider) scope.
fn may_write(access: &EffectiveAccess, permission: &str) -> bool {
    access.allows(permission)
        && access
            .scope_for(permission)
            .is_some_and(|scope| matches!(scope, "institution" | "all"))
}

fn require_write(access: &EffectiveAccess, permission: &str) -> ApiResult<()> {
    if may_write(access, permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

// ---------------------------------------------------------------------------
// Validation (pure, unit tested)
// ---------------------------------------------------------------------------

/// Codes are short, upper-case identifiers ("CSE", "BE-CSBS", "A").
fn normalize_code(raw: &str, what: &str) -> ApiResult<String> {
    let code = raw.trim().to_uppercase();
    if code.is_empty() || code.chars().count() > 32 {
        return Err(ApiError::BadRequest(format!(
            "{what} code is required and must be at most 32 characters"
        )));
    }
    if !code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '&' | '/'))
    {
        return Err(ApiError::BadRequest(format!(
            "{what} code may use letters, digits and - _ . & / only"
        )));
    }
    Ok(code)
}

fn clean_name(raw: &str, what: &str) -> ApiResult<String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 160 {
        return Err(ApiError::BadRequest(format!(
            "{what} name is required and must be at most 160 characters"
        )));
    }
    Ok(name)
}

fn validate_duration_terms(terms: Option<i32>) -> ApiResult<Option<i32>> {
    match terms {
        Some(value) if !(1..=16).contains(&value) => Err(ApiError::BadRequest(
            "duration must be between 1 and 16 semesters".into(),
        )),
        other => Ok(other),
    }
}

fn validate_capacity(capacity: Option<i32>) -> ApiResult<Option<i32>> {
    match capacity {
        Some(value) if !(1..=1000).contains(&value) => Err(ApiError::BadRequest(
            "capacity must be between 1 and 1000".into(),
        )),
        other => Ok(other),
    }
}

fn validate_credits(credits: Option<f64>) -> ApiResult<Option<f64>> {
    match credits {
        Some(value) if !value.is_finite() || !(0.0..=100.0).contains(&value) => Err(
            ApiError::BadRequest("credits must be between 0 and 100".into()),
        ),
        other => Ok(other),
    }
}

/// Whole years a programme runs for; two semesters to a year.
fn duration_years(duration_terms: Option<i32>) -> Option<i32> {
    duration_terms.map(|terms| (terms + 1) / 2)
}

/// The year a class of `year_of_study` started, given the current academic
/// year's start: a 3rd-year class in 2026-27 is the batch that began in 2024.
fn batch_start_year(current_start_year: i32, year_of_study: i32) -> i32 {
    current_start_year - (year_of_study - 1)
}

fn validate_year_of_study(year_of_study: i32, duration_terms: Option<i32>) -> ApiResult<()> {
    let limit = duration_years(duration_terms).unwrap_or(6).clamp(1, 8);
    if (1..=limit).contains(&year_of_study) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "year of study must be between 1 and {limit} for this programme"
        )))
    }
}

fn batch_code(department_code: &str, start_year: i32) -> String {
    format!("{department_code}-{start_year}")
}

fn batch_name(department_code: &str, start_year: i32, years: i32) -> String {
    format!(
        "{department_code} Batch of {start_year}-{}",
        start_year + years.max(1)
    )
}

fn class_name(department_code: &str, year_of_study: i32, section_code: &str) -> String {
    format!("{department_code} - Year {year_of_study} - Section {section_code}")
}

/// Unique-constraint violations become a 409 with a message the
/// administrator can act on; everything else stays a server error.
fn unique_or(error: sqlx::Error, message: &str) -> ApiError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return ApiError::Conflict(message.into());
    }
    error.into()
}

async fn tenant_id(pool: &PgPool, slug: &str) -> ApiResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM platform.tenants WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound("tenant was not found".into()))
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// One class (section) as JSON. `$1` is the tenant id; the caller appends the
/// filter. The year of study is derived from when the batch started relative
/// to the current academic year, so it moves up by itself each year.
const CLASS_JSON_SQL: &str = r#"
    jsonb_build_object(
        'id', section.id,
        'code', section.code,
        'name', section.name,
        'capacity', section.capacity,
        'active', section.active AND batch.active AND programme.active,
        'batchId', batch.id,
        'batchCode', batch.code,
        'batchName', batch.name,
        'startYear', EXTRACT(YEAR FROM COALESCE(batch.starts_on, batch_year.starts_on))::int,
        'yearOfStudy', GREATEST(1,
            EXTRACT(YEAR FROM (SELECT starts_on FROM current_year))::int
            - EXTRACT(YEAR FROM COALESCE(batch.starts_on, batch_year.starts_on))::int + 1),
        'programmeId', programme.id,
        'programmeCode', programme.code,
        'programmeName', programme.name,
        'departmentId', department.id,
        'departmentCode', department.code,
        'departmentName', department.name,
        'studentCount', (
            SELECT count(*) FROM core.students student
            WHERE student.tenant_id = section.tenant_id
              AND student.section_id::text = section.id::text
              AND student.status IN ('provisional', 'active'))
    )"#;

const CLASS_FROM_SQL: &str = r#"
    FROM core.sections section
    JOIN core.batches batch
      ON batch.tenant_id = section.tenant_id AND batch.id = section.batch_id
    JOIN core.academic_years batch_year
      ON batch_year.tenant_id = batch.tenant_id AND batch_year.id = batch.academic_year_id
    JOIN core.programmes programme
      ON programme.tenant_id = batch.tenant_id AND programme.id = batch.programme_id
    JOIN core.departments department
      ON department.tenant_id = programme.tenant_id AND department.id = programme.department_id
    WHERE section.tenant_id = $1"#;

const CURRENT_YEAR_CTE: &str = r#"
    current_year AS (
        SELECT id, code, name, starts_on, ends_on
        FROM core.academic_years
        WHERE tenant_id = $1
        ORDER BY (status = 'active') DESC, starts_on DESC
        LIMIT 1
    )"#;

async fn catalog(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<CatalogQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !may_read(&access) {
        return Err(ApiError::Forbidden);
    }
    let can = json!({
        "createProgrammes": may_write(&access, PROGRAMME_CREATE),
        "updateProgrammes": may_write(&access, PROGRAMME_UPDATE),
        "createSubjects": may_write(&access, SUBJECT_CREATE),
        "updateSubjects": may_write(&access, SUBJECT_UPDATE),
    });
    // Inactive rows are only for the people who can bring them back.
    let include_inactive = query.include_inactive
        && (may_write(&access, PROGRAMME_UPDATE) || may_write(&access, SUBJECT_UPDATE));
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;

    let sql = format!(
        r#"WITH {CURRENT_YEAR_CTE},
           linked AS (
               SELECT department_id::text AS department_id,
                      program_id::text AS program_id
               FROM core.students
               WHERE tenant_id = $1 AND status IN ('provisional', 'active')
           )
           SELECT jsonb_build_object(
               'academicYear', (
                   SELECT jsonb_build_object('id', id, 'code', code, 'name', name,
                                             'startsOn', starts_on, 'endsOn', ends_on)
                   FROM current_year),
               'departments', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', department.id, 'code', department.code,
                       'name', department.name, 'active', department.active,
                       'programmeCount', (
                           SELECT count(*) FROM core.programmes programme
                           WHERE programme.tenant_id = department.tenant_id
                             AND programme.department_id = department.id
                             AND programme.active),
                       'studentCount', (
                           SELECT count(*) FROM linked
                           WHERE linked.department_id = department.id::text)
                   ) ORDER BY department.active DESC, department.name)
                   FROM core.departments department
                   WHERE department.tenant_id = $1 AND ($2 OR department.active)
               ), '[]'::jsonb),
               'programmes', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', programme.id, 'code', programme.code, 'name', programme.name,
                       'departmentId', department.id, 'departmentCode', department.code,
                       'departmentName', department.name,
                       'durationTerms', programme.duration_terms,
                       'durationYears', CASE WHEN programme.duration_terms IS NULL THEN NULL
                                             ELSE (programme.duration_terms + 1) / 2 END,
                       'active', programme.active AND department.active,
                       'classCount', (
                           SELECT count(*) FROM core.sections section
                           JOIN core.batches batch
                             ON batch.tenant_id = section.tenant_id AND batch.id = section.batch_id
                           WHERE batch.tenant_id = programme.tenant_id
                             AND batch.programme_id = programme.id
                             AND section.active AND batch.active),
                       'studentCount', (
                           SELECT count(*) FROM linked
                           WHERE linked.program_id = programme.id::text)
                   ) ORDER BY (programme.active AND department.active) DESC, programme.name)
                   FROM core.programmes programme
                   JOIN core.departments department
                     ON department.tenant_id = programme.tenant_id
                    AND department.id = programme.department_id
                   WHERE programme.tenant_id = $1
                     AND ($2 OR (programme.active AND department.active))
               ), '[]'::jsonb),
               'classes', COALESCE((
                   SELECT jsonb_agg(class_row ORDER BY
                       (class_row ->> 'active')::boolean DESC,
                       class_row ->> 'programmeName',
                       (class_row ->> 'yearOfStudy')::int,
                       class_row ->> 'code')
                   FROM (
                       SELECT {CLASS_JSON_SQL} AS class_row
                       {CLASS_FROM_SQL}
                         AND ($2 OR (section.active AND batch.active
                                     AND programme.active AND department.active))
                   ) classes
               ), '[]'::jsonb),
               'subjects', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', subject.id, 'code', subject.code, 'name', subject.name,
                       'credits', subject.credits::float8,
                       'departmentId', department.id, 'departmentCode', department.code,
                       'departmentName', department.name,
                       'active', subject.active AND department.active,
                       'offeringCount', (
                           SELECT count(*) FROM core.subject_offerings offering
                           WHERE offering.tenant_id = subject.tenant_id
                             AND offering.subject_id = subject.id
                             AND offering.active)
                   ) ORDER BY (subject.active AND department.active) DESC, subject.code)
                   FROM core.subjects subject
                   JOIN core.departments department
                     ON department.tenant_id = subject.tenant_id
                    AND department.id = subject.department_id
                   WHERE subject.tenant_id = $1
                     AND ($2 OR (subject.active AND department.active))
               ), '[]'::jsonb),
               'unlinkedStudentCount', (
                   SELECT count(*) FROM core.students student
                   WHERE student.tenant_id = $1
                     AND student.status IN ('provisional', 'active')
                     AND NOT EXISTS (
                         SELECT 1 FROM core.departments department
                         WHERE department.tenant_id = student.tenant_id
                           AND department.id::text = student.department_id::text))
           )"#
    );
    let mut value = sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(include_inactive)
        .fetch_one(database.pool())
        .await
        .context("failed to load the academic structure")?;
    if let Some(object) = value.as_object_mut() {
        object.insert("can".into(), can);
    }
    Ok(Json(ApiResponse::new(value)))
}

// ---------------------------------------------------------------------------
// Departments
// ---------------------------------------------------------------------------

const DEPARTMENT_RETURNING: &str = "RETURNING jsonb_build_object('id', id, 'code', code, \
     'name', name, 'active', active)";

async fn create_department(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<DepartmentRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_write(&access, PROGRAMME_CREATE)?;
    let code = normalize_code(&request.code, "department")?;
    let name = clean_name(&request.name, "department")?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let value = sqlx::query_scalar::<_, Value>(&format!(
        "INSERT INTO core.departments (tenant_id, code, name, active) \
         VALUES ($1, $2, $3, true) {DEPARTMENT_RETURNING}"
    ))
    .bind(tenant)
    .bind(&code)
    .bind(&name)
    .fetch_one(database.pool())
    .await
    .map_err(|error| unique_or(error, "a department with that code already exists"))?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn update_department(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(department_id): Path<Uuid>,
    Json(request): Json<DepartmentRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_write(&access, PROGRAMME_UPDATE)?;
    let code = normalize_code(&request.code, "department")?;
    let name = clean_name(&request.name, "department")?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    if request.active == Some(false) {
        let blockers = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM core.programmes \
             WHERE tenant_id = $1 AND department_id = $2 AND active",
        )
        .bind(tenant)
        .bind(department_id)
        .fetch_one(&mut *transaction)
        .await?;
        if blockers > 0 {
            return Err(ApiError::Conflict(format!(
                "deactivate the department's {blockers} active programme(s) first"
            )));
        }
    }
    let value = sqlx::query_scalar::<_, Value>(&format!(
        "UPDATE core.departments SET code = $3, name = $4, \
             active = COALESCE($5, active), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 {DEPARTMENT_RETURNING}"
    ))
    .bind(tenant)
    .bind(department_id)
    .bind(&code)
    .bind(&name)
    .bind(request.active)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| unique_or(error, "a department with that code already exists"))?
    .ok_or_else(|| ApiError::NotFound("department was not found".into()))?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(value)))
}

// ---------------------------------------------------------------------------
// Programmes
// ---------------------------------------------------------------------------

const PROGRAMME_RETURNING: &str = "RETURNING jsonb_build_object('id', id, 'code', code, \
     'name', name, 'departmentId', department_id, 'durationTerms', duration_terms, \
     'active', active)";

async fn create_programme(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<ProgrammeRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_write(&access, PROGRAMME_CREATE)?;
    let code = normalize_code(&request.code, "programme")?;
    let name = clean_name(&request.name, "programme")?;
    let duration_terms = validate_duration_terms(request.duration_terms)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let value = sqlx::query_scalar::<_, Value>(&format!(
        "INSERT INTO core.programmes (tenant_id, department_id, code, name, duration_terms, active) \
         SELECT $1, department.id, $3, $4, $5, true \
         FROM core.departments department \
         WHERE department.tenant_id = $1 AND department.id = $2 AND department.active \
         {PROGRAMME_RETURNING}"
    ))
    .bind(tenant)
    .bind(request.department_id)
    .bind(&code)
    .bind(&name)
    .bind(duration_terms)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| unique_or(error, "a programme with that code already exists"))?
    .ok_or_else(|| ApiError::BadRequest("choose an active department".into()))?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn update_programme(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(programme_id): Path<Uuid>,
    Json(request): Json<ProgrammeRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_write(&access, PROGRAMME_UPDATE)?;
    let code = normalize_code(&request.code, "programme")?;
    let name = clean_name(&request.name, "programme")?;
    let duration_terms = validate_duration_terms(request.duration_terms)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    if request.active == Some(false) {
        let students = active_students(
            &mut transaction,
            tenant,
            "program_id::text = $2::text",
            programme_id,
        )
        .await?;
        if students > 0 {
            return Err(ApiError::Conflict(format!(
                "{students} active student(s) are still in this programme; move them first"
            )));
        }
    }
    let value = sqlx::query_scalar::<_, Value>(
        "UPDATE core.programmes programme \
         SET department_id = department.id, code = $4, name = $5, \
             duration_terms = COALESCE($6, programme.duration_terms), \
             active = COALESCE($7, programme.active), updated_at = now() \
         FROM core.departments department \
         WHERE programme.tenant_id = $1 AND programme.id = $2 \
           AND department.tenant_id = $1 AND department.id = $3 \
         RETURNING jsonb_build_object('id', programme.id, 'code', programme.code, \
             'name', programme.name, 'departmentId', programme.department_id, \
             'durationTerms', programme.duration_terms, 'active', programme.active)",
    )
    .bind(tenant)
    .bind(programme_id)
    .bind(request.department_id)
    .bind(&code)
    .bind(&name)
    .bind(duration_terms)
    .bind(request.active)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| unique_or(error, "a programme with that code already exists"))?
    .ok_or_else(|| ApiError::NotFound("programme or department was not found".into()))?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(value)))
}

/// Active students whose Student Master row matches `predicate` (which reads
/// `$2`, the id being deactivated).
async fn active_students(
    transaction: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    predicate: &str,
    id: Uuid,
) -> ApiResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(&format!(
        "SELECT count(*) FROM core.students \
         WHERE tenant_id = $1 AND status IN ('provisional', 'active') AND {predicate}"
    ))
    .bind(tenant)
    .bind(id)
    .fetch_one(&mut **transaction)
    .await?)
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

async fn class_json(
    transaction: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    section_id: Uuid,
) -> ApiResult<Value> {
    sqlx::query_scalar::<_, Value>(&format!(
        "WITH {CURRENT_YEAR_CTE} SELECT {CLASS_JSON_SQL} {CLASS_FROM_SQL} AND section.id = $2"
    ))
    .bind(tenant)
    .bind(section_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| ApiError::NotFound("class was not found".into()))
}

async fn create_class(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateClassRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_write(&access, PROGRAMME_CREATE)?;
    let section_code = normalize_code(&request.section_code, "section")?;
    let capacity = validate_capacity(request.capacity)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;

    let programme = sqlx::query_as::<_, (String, Option<i32>)>(
        "SELECT department.code, programme.duration_terms \
         FROM core.programmes programme \
         JOIN core.departments department \
           ON department.tenant_id = programme.tenant_id \
          AND department.id = programme.department_id \
         WHERE programme.tenant_id = $1 AND programme.id = $2 \
           AND programme.active AND department.active",
    )
    .bind(tenant)
    .bind(request.programme_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ApiError::BadRequest("choose an active programme".into()))?;
    let (department_code, duration_terms) = programme;
    validate_year_of_study(request.year_of_study, duration_terms)?;

    let (current_year_id, current_start_year, current_starts_on) = sqlx::query_as::<
        _,
        (Uuid, i32, chrono::NaiveDate),
    >(
        "SELECT id, EXTRACT(YEAR FROM starts_on)::int, starts_on \
             FROM core.academic_years WHERE tenant_id = $1 \
             ORDER BY (status = 'active') DESC, starts_on DESC LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ApiError::BadRequest("set up an academic year before adding classes".into()))?;
    let start_year = batch_start_year(current_start_year, request.year_of_study);
    let years = duration_years(duration_terms).unwrap_or(4);

    // The yearly batch of this programme that started in `start_year`,
    // reused when it exists (by start date, then by its code).
    let code = batch_code(&department_code, start_year);
    let existing_batch = sqlx::query_scalar::<_, Uuid>(
        "SELECT batch.id FROM core.batches batch \
         JOIN core.academic_years year \
           ON year.tenant_id = batch.tenant_id AND year.id = batch.academic_year_id \
         WHERE batch.tenant_id = $1 AND batch.programme_id = $2 \
           AND (EXTRACT(YEAR FROM COALESCE(batch.starts_on, year.starts_on))::int = $3 \
                OR batch.code = $4) \
         ORDER BY batch.active DESC, (batch.code = $4) DESC LIMIT 1",
    )
    .bind(tenant)
    .bind(request.programme_id)
    .bind(start_year)
    .bind(&code)
    .fetch_optional(&mut *transaction)
    .await?;
    let batch_id = match existing_batch {
        Some(id) => {
            sqlx::query(
                "UPDATE core.batches SET active = true, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2 AND NOT active",
            )
            .bind(tenant)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
            id
        }
        None => {
            // Anchor it to the academic year it began in when that year is
            // on record, otherwise to the current one.
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO core.batches \
                     (tenant_id, programme_id, academic_year_id, code, name, starts_on, ends_on) \
                 VALUES ($1, $2, \
                     COALESCE((SELECT id FROM core.academic_years \
                               WHERE tenant_id = $1 \
                                 AND EXTRACT(YEAR FROM starts_on)::int = $5 \
                               ORDER BY starts_on LIMIT 1), $3), \
                     $4, $6, \
                     ($7::date - make_interval(years => $8)), \
                     ($7::date - make_interval(years => $8) \
                         + make_interval(years => $9) - interval '1 month')::date) \
                 RETURNING id",
            )
            .bind(tenant)
            .bind(request.programme_id)
            .bind(current_year_id)
            .bind(&code)
            .bind(start_year)
            .bind(batch_name(&department_code, start_year, years))
            .bind(current_starts_on)
            .bind(request.year_of_study - 1)
            .bind(years)
            .fetch_one(&mut *transaction)
            .await
            .context("failed to create the class batch")?
        }
    };

    let section_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO core.sections (tenant_id, batch_id, code, name, capacity) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tenant)
    .bind(batch_id)
    .bind(&section_code)
    .bind(class_name(
        &department_code,
        request.year_of_study,
        &section_code,
    ))
    .bind(capacity)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| {
        unique_or(
            error,
            "that section already exists for this programme and year",
        )
    })?;
    let value = class_json(&mut transaction, tenant, section_id).await?;
    transaction.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn update_class(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(class_id): Path<Uuid>,
    Json(request): Json<UpdateClassRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_write(&access, PROGRAMME_UPDATE)?;
    let section_code = normalize_code(&request.section_code, "section")?;
    let capacity = validate_capacity(request.capacity)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    if request.active == Some(false) {
        let students = active_students(
            &mut transaction,
            tenant,
            "section_id::text = $2::text",
            class_id,
        )
        .await?;
        if students > 0 {
            return Err(ApiError::Conflict(format!(
                "{students} active student(s) are still in this class; move them first"
            )));
        }
    }
    // Renaming the section keeps the rest of its display name.
    let updated = sqlx::query(
        "UPDATE core.sections SET \
             name = CASE WHEN code = $3 THEN name \
                         ELSE replace(name, 'Section ' || code, 'Section ' || $3) END, \
             code = $3, capacity = $4, active = COALESCE($5, active), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(class_id)
    .bind(&section_code)
    .bind(capacity)
    .bind(request.active)
    .execute(&mut *transaction)
    .await
    .map_err(|error| {
        unique_or(
            error,
            "that section already exists for this programme and year",
        )
    })?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::NotFound("class was not found".into()));
    }
    let value = class_json(&mut transaction, tenant, class_id).await?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(value)))
}

// ---------------------------------------------------------------------------
// Subjects
// ---------------------------------------------------------------------------

async fn create_subject(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<SubjectRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_write(&access, SUBJECT_CREATE)?;
    let code = normalize_code(&request.code, "subject")?;
    let name = clean_name(&request.name, "subject")?;
    let credits = validate_credits(request.credits)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let value = sqlx::query_scalar::<_, Value>(
        "INSERT INTO core.subjects (tenant_id, department_id, code, name, credits) \
         SELECT $1, department.id, $3, $4, $5 \
         FROM core.departments department \
         WHERE department.tenant_id = $1 AND department.id = $2 AND department.active \
         RETURNING jsonb_build_object('id', id, 'code', code, 'name', name, \
             'departmentId', department_id, 'credits', credits::float8, 'active', active)",
    )
    .bind(tenant)
    .bind(request.department_id)
    .bind(&code)
    .bind(&name)
    .bind(credits)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| unique_or(error, "a subject with that code already exists"))?
    .ok_or_else(|| ApiError::BadRequest("choose an active department".into()))?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn update_subject(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(subject_id): Path<Uuid>,
    Json(request): Json<SubjectRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_write(&access, SUBJECT_UPDATE)?;
    let code = normalize_code(&request.code, "subject")?;
    let name = clean_name(&request.name, "subject")?;
    let credits = validate_credits(request.credits)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(database.pool(), &principal.student.tenant_id).await?;
    let value = sqlx::query_scalar::<_, Value>(
        "UPDATE core.subjects subject \
         SET department_id = department.id, code = $4, name = $5, credits = $6, \
             active = COALESCE($7, subject.active), updated_at = now() \
         FROM core.departments department \
         WHERE subject.tenant_id = $1 AND subject.id = $2 \
           AND department.tenant_id = $1 AND department.id = $3 \
         RETURNING jsonb_build_object('id', subject.id, 'code', subject.code, \
             'name', subject.name, 'departmentId', subject.department_id, \
             'credits', subject.credits::float8, 'active', subject.active)",
    )
    .bind(tenant)
    .bind(subject_id)
    .bind(request.department_id)
    .bind(&code)
    .bind(&name)
    .bind(credits)
    .bind(request.active)
    .fetch_optional(database.pool())
    .await
    .map_err(|error| unique_or(error, "a subject with that code already exists"))?
    .ok_or_else(|| ApiError::NotFound("subject or department was not found".into()))?;
    Ok(Json(ApiResponse::new(value)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn access(grants: &[(&str, &str)]) -> EffectiveAccess {
        EffectiveAccess {
            roles: vec![],
            portal_families: vec!["staff".into()],
            permissions: grants.iter().map(|(key, _)| (*key).to_string()).collect(),
            scopes: grants
                .iter()
                .map(|(key, scope)| ((*key).to_string(), (*scope).to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn reading_the_catalog_follows_grants_not_roles() {
        assert!(may_read(&access(&[("*", "all")])));
        assert!(may_read(&access(&[(
            "students.directory.read",
            "institution"
        )])));
        assert!(may_read(&access(&[("academics.*", "department")])));
        assert!(!may_read(&access(&[("canteen.menu.read", "all")])));
        assert!(!may_read(&access(&[])));
    }

    #[test]
    fn structural_edits_need_institution_scope() {
        assert!(may_write(&access(&[("*", "all")]), PROGRAMME_CREATE));
        assert!(may_write(
            &access(&[(PROGRAMME_UPDATE, "institution")]),
            PROGRAMME_UPDATE
        ));
        assert!(!may_write(
            &access(&[(PROGRAMME_UPDATE, "department")]),
            PROGRAMME_UPDATE
        ));
        assert!(!may_write(
            &access(&[(PROGRAMME_READ, "all")]),
            PROGRAMME_CREATE
        ));
        assert!(!may_write(
            &access(&[(SUBJECT_UPDATE, "all")]),
            PROGRAMME_UPDATE
        ));
    }

    #[test]
    fn codes_are_normalized_and_bounded() {
        assert_eq!(normalize_code(" be-csbs ", "programme").unwrap(), "BE-CSBS");
        assert_eq!(normalize_code("a", "section").unwrap(), "A");
        assert!(normalize_code("   ", "programme").is_err());
        assert!(normalize_code("B E", "programme").is_err());
        assert!(normalize_code(&"X".repeat(33), "programme").is_err());
    }

    #[test]
    fn names_collapse_whitespace() {
        assert_eq!(
            clean_name("  B.E.  Computer   Science ", "programme").unwrap(),
            "B.E. Computer Science"
        );
        assert!(clean_name(" ", "programme").is_err());
    }

    #[test]
    fn class_year_maps_to_the_batch_that_started_then() {
        assert_eq!(batch_start_year(2026, 1), 2026);
        assert_eq!(batch_start_year(2026, 3), 2024);
        assert_eq!(duration_years(Some(8)), Some(4));
        assert_eq!(duration_years(Some(5)), Some(3));
        assert_eq!(duration_years(None), None);
        assert_eq!(batch_code("CSBS", 2025), "CSBS-2025");
        assert_eq!(batch_name("CSBS", 2025, 4), "CSBS Batch of 2025-2029");
        assert_eq!(class_name("CSBS", 2, "B"), "CSBS - Year 2 - Section B");
    }

    #[test]
    fn year_of_study_stays_within_the_programme() {
        assert!(validate_year_of_study(4, Some(8)).is_ok());
        assert!(validate_year_of_study(5, Some(8)).is_err());
        assert!(validate_year_of_study(0, Some(8)).is_err());
        assert!(validate_year_of_study(6, None).is_ok());
    }

    #[test]
    fn numeric_fields_are_bounded() {
        assert!(validate_duration_terms(Some(8)).is_ok());
        assert!(validate_duration_terms(Some(0)).is_err());
        assert!(validate_duration_terms(None).unwrap().is_none());
        assert!(validate_capacity(Some(60)).is_ok());
        assert!(validate_capacity(Some(0)).is_err());
        assert!(validate_credits(Some(4.0)).is_ok());
        assert!(validate_credits(Some(-1.0)).is_err());
        assert!(validate_credits(Some(f64::NAN)).is_err());
    }
}
