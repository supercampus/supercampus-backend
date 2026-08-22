use anyhow::Context;
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    governance::{GovernedCapability, any_role_may_perform},
    models::ApiResponse,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/context", get(context))
        .route("/subjects", post(create_subject))
        .route("/offerings", post(create_offering))
        .route("/hod", post(assign_hod))
        .route("/hod/{assignment_id}", delete(remove_hod))
        .route("/teaching", post(assign_teaching))
        .route("/teaching/{assignment_id}", delete(remove_teaching))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSubjectRequest {
    department_id: Uuid,
    code: String,
    name: String,
    credits: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateOfferingRequest {
    subject_id: Uuid,
    academic_year_id: Uuid,
    term_id: Option<Uuid>,
    section_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssignHodRequest {
    user_id: Uuid,
    department_id: Uuid,
    starts_on: Option<NaiveDate>,
    ends_on: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssignTeachingRequest {
    faculty_user_id: Uuid,
    faculty_department_id: Uuid,
    subject_offering_id: Uuid,
    #[serde(default = "default_assignment_type")]
    assignment_type: String,
}

fn default_assignment_type() -> String {
    "primary".into()
}

async fn context(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !access.allows("academics.assignments.read")
        && !access.allows("academics.assignments.manage")
    {
        return Err(ApiError::Forbidden);
    }
    let scope = access
        .scope_for("academics.assignments.read")
        .or_else(|| access.scope_for("academics.assignments.manage"))
        .unwrap_or("assigned");
    let institution_wide = matches!(scope, "institution" | "all");
    let department_wide = scope == "department";
    let can_manage = access.allows("academics.assignments.manage")
        && access
            .scope_for("academics.assignments.manage")
            .is_some_and(|scope| matches!(scope, "institution" | "all"));
    let user_id = parse_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;

    let value = sqlx::query_scalar::<_, Value>(
        r#"WITH tenant AS (
               SELECT id FROM platform.tenants WHERE slug = $1
           ), member_departments AS (
               SELECT authority.department_id
               FROM core.department_authorities authority, tenant
               WHERE authority.tenant_id = tenant.id
                 AND authority.user_id = $2
                 AND authority.active
                 AND (authority.starts_on IS NULL OR authority.starts_on <= CURRENT_DATE)
                 AND (authority.ends_on IS NULL OR authority.ends_on >= CURRENT_DATE)
               UNION
               SELECT employee.department_id
               FROM core.employees employee, tenant
               WHERE employee.tenant_id = tenant.id
                 AND employee.user_id = $2
                 AND employee.status = 'active'
                 AND employee.department_id IS NOT NULL
           ), member_sections AS (
               SELECT enrollment.section_id
               FROM core.students student
               JOIN tenant ON tenant.id = student.tenant_id
               JOIN core.academic_enrollments enrollment
                 ON enrollment.tenant_id = student.tenant_id
                AND enrollment.student_id = student.id
                AND enrollment.status IN ('provisional', 'active')
               WHERE student.user_account_id = $2
                 AND student.status IN ('provisional', 'active')
                 AND enrollment.section_id IS NOT NULL
           ), visible_offerings AS (
               SELECT DISTINCT offering.id
               FROM core.subject_offerings offering
               JOIN tenant ON tenant.id = offering.tenant_id
               JOIN core.sections section
                 ON section.tenant_id = offering.tenant_id AND section.id = offering.section_id
               JOIN core.batches batch
                 ON batch.tenant_id = section.tenant_id AND batch.id = section.batch_id
               JOIN core.programmes programme
                 ON programme.tenant_id = batch.tenant_id AND programme.id = batch.programme_id
               WHERE offering.active AND (
                   $3
                   OR offering.section_id IN (SELECT section_id FROM member_sections)
                   OR EXISTS (
                       SELECT 1 FROM core.teaching_assignments mine
                       WHERE mine.tenant_id = offering.tenant_id
                         AND mine.subject_offering_id = offering.id
                         AND mine.faculty_user_id = $2 AND mine.active
                   )
                   OR ($4 AND (
                       programme.department_id IN (SELECT department_id FROM member_departments)
                       OR EXISTS (
                           SELECT 1
                           FROM core.teaching_assignments teaching
                           JOIN core.employees faculty
                             ON faculty.tenant_id = teaching.tenant_id
                            AND faculty.user_id = teaching.faculty_user_id
                            AND faculty.status = 'active'
                           WHERE teaching.tenant_id = offering.tenant_id
                             AND teaching.subject_offering_id = offering.id
                             AND teaching.active
                             AND faculty.department_id IN (SELECT department_id FROM member_departments)
                       )
                   ))
               )
           )
           SELECT jsonb_build_object(
               'scope', $5::text,
               'institutionWide', $3::boolean,
               'includesCrossDepartmentStaffTeaching', $4::boolean,
               'canManage', $6::boolean,
               'academicYears', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', year.id, 'code', year.code, 'name', year.name,
                       'startsOn', year.starts_on, 'endsOn', year.ends_on,
                       'status', year.status
                   ) ORDER BY year.starts_on DESC)
                   FROM core.academic_years year, tenant
                   WHERE year.tenant_id = tenant.id
               ), '[]'::jsonb),
               'terms', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', term.id, 'academicYearId', term.academic_year_id,
                       'code', term.code, 'name', term.name,
                       'sequence', term.sequence, 'status', term.status
                   ) ORDER BY term.academic_year_id, term.sequence)
                   FROM core.terms term, tenant
                   WHERE term.tenant_id = tenant.id
               ), '[]'::jsonb),
               'departments', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', department.id, 'code', department.code, 'name', department.name
                   ) ORDER BY department.name)
                   FROM core.departments department, tenant
                   WHERE department.tenant_id = tenant.id AND department.active
                     AND ($3 OR department.id IN (SELECT department_id FROM member_departments)
                         OR EXISTS (
                             SELECT 1 FROM visible_offerings visible
                             JOIN core.subject_offerings offering ON offering.id = visible.id
                             JOIN core.subjects subject ON subject.id = offering.subject_id
                             JOIN core.sections section ON section.id = offering.section_id
                             JOIN core.batches batch ON batch.id = section.batch_id
                             JOIN core.programmes programme ON programme.id = batch.programme_id
                             WHERE subject.department_id = department.id
                                OR programme.department_id = department.id
                         ))
               ), '[]'::jsonb),
               'sections', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', section.id, 'code', section.code, 'name', section.name,
                       'batchId', batch.id, 'batchName', batch.name,
                       'programmeId', programme.id, 'programmeName', programme.name,
                       'departmentId', programme.department_id
                   ) ORDER BY programme.name, batch.name, section.name)
                   FROM core.sections section
                   JOIN tenant ON tenant.id = section.tenant_id
                   JOIN core.batches batch
                     ON batch.tenant_id = section.tenant_id AND batch.id = section.batch_id
                   JOIN core.programmes programme
                     ON programme.tenant_id = batch.tenant_id AND programme.id = batch.programme_id
                   WHERE section.active AND batch.active AND programme.active
                     AND ($3 OR section.id IN (SELECT section_id FROM member_sections)
                         OR section.id IN (
                             SELECT offering.section_id
                             FROM visible_offerings visible
                             JOIN core.subject_offerings offering ON offering.id = visible.id
                         ))
               ), '[]'::jsonb),
               'subjects', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', subject.id, 'departmentId', subject.department_id,
                       'code', subject.code, 'name', subject.name, 'credits', subject.credits
                   ) ORDER BY subject.code)
                   FROM core.subjects subject, tenant
                   WHERE subject.tenant_id = tenant.id AND subject.active
                     AND ($3 OR EXISTS (
                         SELECT 1 FROM core.subject_offerings offering
                         JOIN visible_offerings visible ON visible.id = offering.id
                         WHERE offering.subject_id = subject.id
                     ))
               ), '[]'::jsonb),
               'offerings', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', offering.id, 'subjectId', offering.subject_id,
                       'subjectCode', subject.code, 'subjectName', subject.name,
                       'academicYearId', offering.academic_year_id, 'termId', offering.term_id,
                       'sectionId', section.id, 'sectionName', section.name,
                       'programmeId', programme.id, 'programmeName', programme.name,
                       'departmentId', programme.department_id
                   ) ORDER BY subject.code, section.name)
                   FROM visible_offerings visible
                   JOIN core.subject_offerings offering ON offering.id = visible.id
                   JOIN core.subjects subject ON subject.id = offering.subject_id
                   JOIN core.sections section ON section.id = offering.section_id
                   JOIN core.batches batch ON batch.id = section.batch_id
                   JOIN core.programmes programme ON programme.id = batch.programme_id
               ), '[]'::jsonb),
               'departmentAuthorities', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', authority.id, 'departmentId', authority.department_id,
                       'userId', authority.user_id, 'name', user_account.display_name,
                       'email', user_account.email, 'startsOn', authority.starts_on,
                       'endsOn', authority.ends_on
                   ) ORDER BY department.name, user_account.display_name)
                   FROM core.department_authorities authority
                   JOIN tenant ON tenant.id = authority.tenant_id
                   JOIN core.departments department ON department.id = authority.department_id
                   JOIN identity.users user_account ON user_account.id = authority.user_id
                   WHERE authority.active
                     AND ($3 OR authority.department_id IN (SELECT department_id FROM member_departments))
               ), '[]'::jsonb),
               'teachingAssignments', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', teaching.id, 'subjectOfferingId', teaching.subject_offering_id,
                       'facultyUserId', teaching.faculty_user_id,
                       'facultyName', user_account.display_name, 'facultyEmail', user_account.email,
                       'facultyDepartmentId', employee.department_id,
                       'assignmentType', teaching.assignment_type
                   ) ORDER BY user_account.display_name)
                   FROM core.teaching_assignments teaching
                   JOIN visible_offerings visible ON visible.id = teaching.subject_offering_id
                   JOIN identity.users user_account ON user_account.id = teaching.faculty_user_id
                   LEFT JOIN core.employees employee
                     ON employee.tenant_id = teaching.tenant_id
                    AND employee.user_id = teaching.faculty_user_id
                   WHERE teaching.active
               ), '[]'::jsonb),
               'eligibleHods', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', user_account.id, 'name', user_account.display_name,
                       'email', user_account.email
                   ) ORDER BY user_account.display_name)
                   FROM identity.tenant_memberships membership
                   JOIN tenant ON tenant.id = membership.tenant_id
                   JOIN identity.users user_account
                     ON user_account.id = membership.user_id AND user_account.active
                   WHERE $6 AND membership.active AND 'hod' = ANY(membership.roles)
               ), '[]'::jsonb),
               'eligibleFaculty', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', user_account.id, 'name', user_account.display_name,
                       'email', user_account.email, 'departmentId', employee.department_id
                   ) ORDER BY user_account.display_name)
                   FROM identity.tenant_memberships membership
                   JOIN tenant ON tenant.id = membership.tenant_id
                   JOIN identity.users user_account
                     ON user_account.id = membership.user_id AND user_account.active
                   LEFT JOIN core.employees employee
                     ON employee.tenant_id = membership.tenant_id
                    AND employee.user_id = membership.user_id
                   WHERE $6 AND membership.active AND 'faculty' = ANY(membership.roles)
               ), '[]'::jsonb)
           )"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(user_id)
    .bind(institution_wide)
    .bind(department_wide)
    .bind(scope)
    .bind(can_manage)
    .fetch_one(database.pool())
    .await
    .context("failed to resolve academic assignment visibility")?;

    Ok(Json(ApiResponse::new(value)))
}

async fn create_subject(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateSubjectRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_assignment_manager(&principal, &access)?;
    if request.code.trim().is_empty()
        || request.name.trim().is_empty()
        || request.credits.is_some_and(|credits| credits < 0.0)
    {
        return Err(ApiError::BadRequest(
            "subject code and name are required, and credits cannot be negative".into(),
        ));
    }
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let row = sqlx::query(
        r#"INSERT INTO core.subjects (tenant_id, department_id, code, name, credits)
           SELECT tenant.id, department.id, $3, $4, $5
           FROM platform.tenants tenant
           JOIN core.departments department
             ON department.tenant_id = tenant.id AND department.id = $2 AND department.active
           WHERE tenant.slug = $1
           RETURNING id, department_id, code, name, credits::float8 AS credits"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.department_id)
    .bind(request.code.trim())
    .bind(request.name.trim())
    .bind(request.credits)
    .fetch_optional(database.pool())
    .await
    .context("failed to create subject")?
    .ok_or_else(|| ApiError::BadRequest("department does not belong to this tenant".into()))?;
    let value = json!({
        "id": row.try_get::<Uuid, _>("id")?,
        "departmentId": row.try_get::<Uuid, _>("department_id")?,
        "code": row.try_get::<String, _>("code")?,
        "name": row.try_get::<String, _>("name")?,
        "credits": row.try_get::<Option<f64>, _>("credits")?,
    });
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn create_offering(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateOfferingRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_assignment_manager(&principal, &access)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO core.subject_offerings
               (tenant_id, subject_id, academic_year_id, term_id, section_id)
           SELECT tenant.id, subject.id, academic_year.id, term.id, section.id
           FROM platform.tenants tenant
           JOIN core.subjects subject
             ON subject.tenant_id = tenant.id AND subject.id = $2 AND subject.active
           JOIN core.academic_years academic_year
             ON academic_year.tenant_id = tenant.id AND academic_year.id = $3
           JOIN core.sections section
             ON section.tenant_id = tenant.id AND section.id = $5 AND section.active
           LEFT JOIN core.terms term
             ON term.tenant_id = tenant.id AND term.id = $4
           WHERE tenant.slug = $1 AND ($4 IS NULL OR term.id IS NOT NULL)
           ON CONFLICT DO NOTHING
           RETURNING id"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.subject_id)
    .bind(request.academic_year_id)
    .bind(request.term_id)
    .bind(request.section_id)
    .fetch_optional(database.pool())
    .await
    .context("failed to create subject offering")?;
    let id = match id {
        Some(id) => id,
        None => sqlx::query_scalar::<_, Uuid>(
            r#"UPDATE core.subject_offerings offering
               SET active = true, updated_at = now()
               FROM platform.tenants tenant
               WHERE tenant.id = offering.tenant_id AND tenant.slug = $1
                 AND offering.subject_id = $2
                 AND offering.academic_year_id = $3
                 AND offering.term_id IS NOT DISTINCT FROM $4
                 AND offering.section_id = $5
               RETURNING offering.id"#,
        )
        .bind(&principal.student.tenant_id)
        .bind(request.subject_id)
        .bind(request.academic_year_id)
        .bind(request.term_id)
        .bind(request.section_id)
        .fetch_optional(database.pool())
        .await
        .context("failed to resolve existing subject offering")?
        .ok_or_else(|| {
            ApiError::BadRequest(
                "subject, academic year, term, or section is outside this tenant".into(),
            )
        })?,
    };
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::new(json!({ "id": id }))),
    ))
}

async fn assign_hod(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<AssignHodRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_assignment_manager(&principal, &access)?;
    if request
        .ends_on
        .zip(request.starts_on)
        .is_some_and(|(ends_on, starts_on)| ends_on < starts_on)
    {
        return Err(ApiError::BadRequest(
            "endsOn cannot be earlier than startsOn".into(),
        ));
    }
    let actor_id = parse_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    ensure_target_role(
        &mut transaction,
        &principal.student.tenant_id,
        request.user_id,
        "hod",
    )
    .await?;
    let previous = existing_state(
        &mut transaction,
        "core.department_authorities",
        &principal.student.tenant_id,
        request.department_id,
        request.user_id,
    )
    .await?;
    let row = sqlx::query(
        r#"INSERT INTO core.department_authorities
               (tenant_id, department_id, user_id, authority_role, starts_on, ends_on, assigned_by)
           SELECT tenant.id, department.id, $3, 'hod', $4, $5, $6
           FROM platform.tenants tenant
           JOIN core.departments department
             ON department.tenant_id = tenant.id AND department.id = $2 AND department.active
           WHERE tenant.slug = $1
           ON CONFLICT (tenant_id, department_id, user_id, authority_role)
           DO UPDATE SET starts_on = EXCLUDED.starts_on, ends_on = EXCLUDED.ends_on,
                         assigned_by = EXCLUDED.assigned_by, active = true, updated_at = now()
           RETURNING id, to_jsonb(core.department_authorities.*) AS document"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.department_id)
    .bind(request.user_id)
    .bind(request.starts_on)
    .bind(request.ends_on)
    .bind(actor_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("failed to assign HOD")?
    .ok_or_else(|| ApiError::BadRequest("department does not belong to this tenant".into()))?;
    let id: Uuid = row.try_get("id")?;
    let document: Value = row.try_get("document")?;
    audit_assignment(
        &mut transaction,
        &principal.student.tenant_id,
        actor_id,
        "department_authority",
        id,
        if previous.is_some() {
            "updated"
        } else {
            "assigned"
        },
        previous,
        Some(document.clone()),
    )
    .await?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(document)))
}

async fn assign_teaching(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<AssignTeachingRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_assignment_manager(&principal, &access)?;
    if !matches!(
        request.assignment_type.as_str(),
        "primary" | "co_faculty" | "substitute"
    ) {
        return Err(ApiError::BadRequest(
            "assignmentType must be primary, co_faculty, or substitute".into(),
        ));
    }
    let actor_id = parse_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    ensure_target_role(
        &mut transaction,
        &principal.student.tenant_id,
        request.faculty_user_id,
        "faculty",
    )
    .await?;
    ensure_faculty_employee(
        &mut transaction,
        &principal.student.tenant_id,
        request.faculty_user_id,
        request.faculty_department_id,
    )
    .await?;
    let previous = sqlx::query_scalar::<_, Value>(
        r#"SELECT to_jsonb(teaching.*)
           FROM core.teaching_assignments teaching
           JOIN platform.tenants tenant ON tenant.id = teaching.tenant_id
           WHERE tenant.slug = $1 AND teaching.subject_offering_id = $2
             AND teaching.faculty_user_id = $3 AND teaching.assignment_type = $4"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.subject_offering_id)
    .bind(request.faculty_user_id)
    .bind(&request.assignment_type)
    .fetch_optional(&mut *transaction)
    .await?;
    let row = sqlx::query(
        r#"INSERT INTO core.teaching_assignments
               (tenant_id, subject_offering_id, faculty_user_id, assignment_type, assigned_by)
           SELECT tenant.id, offering.id, $3, $4, $5
           FROM platform.tenants tenant
           JOIN core.subject_offerings offering
             ON offering.tenant_id = tenant.id AND offering.id = $2 AND offering.active
           WHERE tenant.slug = $1
           ON CONFLICT (tenant_id, subject_offering_id, faculty_user_id, assignment_type)
           DO UPDATE SET assigned_by = EXCLUDED.assigned_by, active = true, updated_at = now()
           RETURNING id, to_jsonb(core.teaching_assignments.*) AS document"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.subject_offering_id)
    .bind(request.faculty_user_id)
    .bind(&request.assignment_type)
    .bind(actor_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("failed to assign Faculty teaching")?
    .ok_or_else(|| {
        ApiError::BadRequest("subject offering does not belong to this tenant".into())
    })?;
    let id: Uuid = row.try_get("id")?;
    let document: Value = row.try_get("document")?;
    audit_assignment(
        &mut transaction,
        &principal.student.tenant_id,
        actor_id,
        "teaching_assignment",
        id,
        if previous.is_some() {
            "updated"
        } else {
            "assigned"
        },
        previous,
        Some(document.clone()),
    )
    .await?;
    transaction.commit().await?;
    Ok(Json(ApiResponse::new(document)))
}

async fn remove_hod(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(assignment_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    remove_assignment(
        &state,
        &principal,
        &access,
        "core.department_authorities",
        "department_authority",
        assignment_id,
    )
    .await
}

async fn remove_teaching(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(assignment_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    remove_assignment(
        &state,
        &principal,
        &access,
        "core.teaching_assignments",
        "teaching_assignment",
        assignment_id,
    )
    .await
}

async fn remove_assignment(
    state: &AppState,
    principal: &AuthPrincipal,
    access: &EffectiveAccess,
    table: &str,
    resource_type: &str,
    assignment_id: Uuid,
) -> ApiResult<StatusCode> {
    require_assignment_manager(principal, access)?;
    let actor_id = parse_user_id(principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut transaction = database.pool().begin().await?;
    let select_sql = format!(
        "SELECT to_jsonb(assignment.*) FROM {table} assignment JOIN platform.tenants tenant ON tenant.id = assignment.tenant_id WHERE tenant.slug = $1 AND assignment.id = $2 AND assignment.active"
    );
    let previous = sqlx::query_scalar::<_, Value>(&select_sql)
        .bind(&principal.student.tenant_id)
        .bind(assignment_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("failed to find academic assignment")?
        .ok_or_else(|| ApiError::NotFound("academic assignment was not found".into()))?;
    let sql = format!(
        "UPDATE {table} assignment SET active = false, updated_at = now() FROM platform.tenants tenant WHERE tenant.id = assignment.tenant_id AND tenant.slug = $1 AND assignment.id = $2 AND assignment.active"
    );
    let updated = sqlx::query(&sql)
        .bind(&principal.student.tenant_id)
        .bind(assignment_id)
        .execute(&mut *transaction)
        .await
        .context("failed to remove academic assignment")?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::Conflict(
            "academic assignment changed before it could be removed".into(),
        ));
    }
    audit_assignment(
        &mut transaction,
        &principal.student.tenant_id,
        actor_id,
        resource_type,
        assignment_id,
        "removed",
        Some(previous),
        None,
    )
    .await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn require_assignment_manager(
    principal: &AuthPrincipal,
    access: &EffectiveAccess,
) -> ApiResult<()> {
    if !access.allows("academics.assignments.manage")
        || !access
            .scope_for("academics.assignments.manage")
            .is_some_and(|scope| matches!(scope, "institution" | "all"))
        || !any_role_may_perform(
            principal.roles.iter().map(String::as_str),
            GovernedCapability::AcademicAssignmentManagement,
        )
    {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

fn parse_user_id(principal: &AuthPrincipal) -> ApiResult<Uuid> {
    Uuid::parse_str(&principal.student.id).map_err(|_| ApiError::Internal)
}

async fn ensure_target_role(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_slug: &str,
    user_id: Uuid,
    role_key: &str,
) -> ApiResult<()> {
    let valid = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
               FROM platform.tenants tenant
               JOIN identity.tenant_memberships membership
                 ON membership.tenant_id = tenant.id
                AND membership.user_id = $2 AND membership.active
               WHERE tenant.slug = $1 AND $3 = ANY(membership.roles)
           )"#,
    )
    .bind(tenant_slug)
    .bind(user_id)
    .bind(role_key)
    .fetch_one(&mut **transaction)
    .await?;
    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "target user must be an active {role_key} in this tenant"
        )))
    }
}

async fn ensure_faculty_employee(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_slug: &str,
    user_id: Uuid,
    department_id: Uuid,
) -> ApiResult<()> {
    let inserted = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO core.employees
               (tenant_id, user_id, employee_number, department_id, full_name, email, status, profile)
           SELECT tenant.id, user_account.id,
                  'STAFF-' || upper(substr(replace(user_account.id::text, '-', ''), 1, 10)),
                  department.id, user_account.display_name, user_account.email, 'active',
                  jsonb_build_object('source', 'academic_assignment')
           FROM platform.tenants tenant
           JOIN identity.users user_account ON user_account.id = $2 AND user_account.active
           JOIN core.departments department
             ON department.tenant_id = tenant.id AND department.id = $3 AND department.active
           WHERE tenant.slug = $1
           ON CONFLICT (tenant_id, user_id) DO UPDATE SET
               department_id = EXCLUDED.department_id,
               full_name = EXCLUDED.full_name,
               email = EXCLUDED.email,
               status = 'active',
               updated_at = now()
           RETURNING id"#,
    )
    .bind(tenant_slug)
    .bind(user_id)
    .bind(department_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if inserted.is_some() {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "faculty department does not belong to this tenant".into(),
        ))
    }
}

async fn existing_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    tenant_slug: &str,
    department_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<Option<Value>> {
    let sql = format!(
        "SELECT to_jsonb(assignment.*) FROM {table} assignment JOIN platform.tenants tenant ON tenant.id = assignment.tenant_id WHERE tenant.slug = $1 AND assignment.department_id = $2 AND assignment.user_id = $3"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(tenant_slug)
        .bind(department_id)
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn audit_assignment(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_slug: &str,
    actor_user_id: Uuid,
    resource_type: &str,
    resource_id: Uuid,
    action: &str,
    before_state: Option<Value>,
    after_state: Option<Value>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO core.academic_assignment_audit
               (tenant_id, actor_user_id, resource_type, resource_id, action,
                before_state, after_state)
           SELECT tenant.id, $2, $3, $4, $5, $6, $7
           FROM platform.tenants tenant WHERE tenant.slug = $1"#,
    )
    .bind(tenant_slug)
    .bind(actor_user_id)
    .bind(resource_type)
    .bind(resource_id)
    .bind(action)
    .bind(before_state)
    .bind(after_state)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::require_assignment_manager;
    use crate::{
        models::{AuthStudent, TenantSummary},
        state::{AuthPrincipal, EffectiveAccess},
    };
    use uuid::Uuid;

    fn principal(role: &str) -> AuthPrincipal {
        AuthPrincipal {
            session_id: Uuid::new_v4(),
            student: AuthStudent {
                id: Uuid::new_v4().to_string(),
                tenant_id: "tenant-local".into(),
                email: "test@example.edu".into(),
                name: "Test".into(),
                initials: "T".into(),
                role: role.into(),
                portal_families: vec!["staff".into()],
                team: "Academics".into(),
                access: vec![],
                roll: String::new(),
                college: String::new(),
                dept: String::new(),
                year: String::new(),
                full_college: String::new(),
                tenant: TenantSummary {
                    id: "tenant-local".into(),
                    code: "LOCAL".into(),
                    name: "Local".into(),
                    city: String::new(),
                },
            },
            roles: vec![role.into()],
        }
    }

    fn access(permission: bool) -> EffectiveAccess {
        let mut scopes = HashMap::new();
        if permission {
            scopes.insert("academics.assignments.manage".into(), "institution".into());
        }
        EffectiveAccess {
            roles: vec![],
            portal_families: vec!["staff".into()],
            permissions: if permission {
                vec!["academics.assignments.manage".into()]
            } else {
                Vec::new()
            },
            scopes,
        }
    }

    #[test]
    fn assignment_writes_need_permission_and_governance_role() {
        assert!(require_assignment_manager(&principal("principal"), &access(true)).is_ok());
        assert!(
            require_assignment_manager(&principal("academic_administrator"), &access(true)).is_ok()
        );
        assert!(require_assignment_manager(&principal("hod"), &access(true)).is_err());
        assert!(require_assignment_manager(&principal("principal"), &access(false)).is_err());
    }

    #[test]
    fn permission_scope_uses_the_broadest_matching_grant() {
        let mut effective = access(true);
        effective
            .scopes
            .insert("academics.*".into(), "department".into());
        effective.scopes.insert("*".into(), "assigned".into());

        assert_eq!(
            effective.scope_for("academics.assignments.manage"),
            Some("institution")
        );
    }
}
