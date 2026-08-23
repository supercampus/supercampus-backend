use anyhow::Context;
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
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
        .route("/changes", get(changes))
        .route("/configurations", post(create_configuration))
        .route(
            "/configurations/{configuration_id}/slots",
            put(replace_slots),
        )
        .route("/rooms", post(create_room))
        .route("/rooms/bulk", post(create_rooms_bulk))
        .route("/workload-requirements", put(upsert_workload_requirement))
        .route("/elective-groups", post(create_elective_group))
        .route("/versions", post(create_version))
        .route("/entries", post(create_entry))
        .route("/versions/{version_id}/publish", post(publish_version))
        .route("/substitutions", post(request_substitution))
        .route(
            "/substitutions/{request_id}/acknowledge",
            post(acknowledge_substitution),
        )
        .route(
            "/substitutions/{request_id}/decision",
            post(decide_substitution),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateConfigurationRequest {
    academic_year_id: Uuid,
    term_id: Option<Uuid>,
    name: String,
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default = "default_working_days")]
    working_days: Vec<i16>,
    #[serde(default = "default_max_periods")]
    max_faculty_periods_per_day: i16,
    #[serde(default = "default_max_consecutive")]
    max_consecutive_faculty_periods: i16,
    #[serde(default)]
    rules: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SlotInput {
    day_of_week: i16,
    sequence: i16,
    label: String,
    slot_type: String,
    starts_at: NaiveTime,
    ends_at: NaiveTime,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceSlotsRequest {
    slots: Vec<SlotInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRoomRequest {
    campus_id: Option<Uuid>,
    department_id: Option<Uuid>,
    department_code: Option<String>,
    code: String,
    name: String,
    room_type: String,
    capacity: i32,
    #[serde(default)]
    features: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRoomsBulkRequest {
    rooms: Vec<CreateRoomRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpsertWorkloadRequirementRequest {
    subject_offering_id: Uuid,
    #[serde(default = "default_delivery_type")]
    delivery_type: String,
    periods_per_week: i16,
    #[serde(default = "default_block_size")]
    block_size: i16,
    #[serde(default = "default_max_blocks_per_day")]
    max_blocks_per_day: i16,
    #[serde(default)]
    required_room_types: Vec<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateElectiveGroupRequest {
    academic_year_id: Uuid,
    term_id: Option<Uuid>,
    code: String,
    name: String,
    #[serde(default)]
    section_ids: Vec<Uuid>,
    #[serde(default)]
    student_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVersionRequest {
    configuration_id: Uuid,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateEntryRequest {
    version_id: Uuid,
    slot_id: Uuid,
    subject_offering_id: Uuid,
    teaching_assignment_id: Uuid,
    room_id: Uuid,
    elective_group_id: Option<Uuid>,
    #[serde(default = "default_delivery_type")]
    delivery_type: String,
    session_block_id: Option<Uuid>,
    #[serde(default = "default_block_size")]
    block_sequence: i16,
    #[serde(default = "default_block_size")]
    block_length: i16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestSubstitutionRequest {
    timetable_entry_id: Uuid,
    service_date: NaiveDate,
    substitute_faculty_user_id: Uuid,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcknowledgeSubstitutionRequest {
    #[serde(default)]
    evidence: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecideSubstitutionRequest {
    decision: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangeCursor {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_change_limit")]
    limit: i64,
}

fn default_timezone() -> String {
    "Asia/Kolkata".into()
}
fn default_working_days() -> Vec<i16> {
    vec![1, 2, 3, 4, 5]
}
fn default_max_periods() -> i16 {
    6
}
fn default_max_consecutive() -> i16 {
    3
}
fn default_delivery_type() -> String {
    "class".into()
}
fn default_block_size() -> i16 {
    1
}
fn default_max_blocks_per_day() -> i16 {
    1
}
fn default_change_limit() -> i64 {
    100
}

async fn context(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_read(&access)?;
    let user_id = principal_user_id(&principal)?;
    let scope = read_scope(&access);
    let institution_wide = matches!(scope, "institution" | "all");
    let department_wide = scope == "department";
    let can_manage = timetable_manager_allowed(&principal, &access);
    let database = state.tenant_database(&principal.student.tenant_id).await?;

    let value = sqlx::query_scalar::<_, Value>(
        r#"WITH tenant AS (
               SELECT id FROM platform.tenants WHERE slug = $1
           ), member_departments AS (
               SELECT department_id FROM core.department_authorities authority, tenant
               WHERE authority.tenant_id = tenant.id AND authority.user_id = $2
                 AND authority.active
                 AND (authority.starts_on IS NULL OR authority.starts_on <= CURRENT_DATE)
                 AND (authority.ends_on IS NULL OR authority.ends_on >= CURRENT_DATE)
               UNION
               SELECT department_id FROM core.employees employee, tenant
               WHERE employee.tenant_id = tenant.id AND employee.user_id = $2
                 AND employee.status = 'active' AND department_id IS NOT NULL
           ), member_sections AS (
               SELECT enrollment.section_id
               FROM core.students student
               JOIN tenant ON tenant.id = student.tenant_id
               JOIN core.academic_enrollments enrollment
                 ON enrollment.tenant_id = student.tenant_id
                AND enrollment.student_id = student.id
                AND enrollment.status IN ('provisional', 'active')
               WHERE student.user_account_id = $2 AND student.status IN ('provisional', 'active')
           ), visible_entries AS (
               SELECT DISTINCT entry.id
               FROM core.timetable_entries entry
               JOIN tenant ON tenant.id = entry.tenant_id
               JOIN core.timetable_versions version ON version.id = entry.version_id
               JOIN core.subject_offerings offering ON offering.id = entry.subject_offering_id
               JOIN core.sections section ON section.id = offering.section_id
               JOIN core.batches batch ON batch.id = section.batch_id
               JOIN core.programmes programme ON programme.id = batch.programme_id
               JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
               LEFT JOIN core.employees faculty
                 ON faculty.tenant_id = entry.tenant_id AND faculty.user_id = teaching.faculty_user_id
               WHERE version.status = 'published' AND (
                   $3 OR offering.section_id IN (SELECT section_id FROM member_sections)
                   OR EXISTS (
                       SELECT 1 FROM core.elective_group_students member
                       JOIN core.students student ON student.id = member.student_id
                       WHERE member.tenant_id = entry.tenant_id
                         AND member.elective_group_id = entry.elective_group_id
                         AND student.user_account_id = $2
                   )
                   OR teaching.faculty_user_id = $2
                   OR EXISTS (
                       SELECT 1 FROM core.faculty_substitution_requests substitution
                       WHERE substitution.tenant_id = entry.tenant_id
                         AND substitution.timetable_entry_id = entry.id
                         AND substitution.substitute_faculty_user_id = $2
                         AND substitution.status = 'approved'
                   )
                   OR ($4 AND (
                       programme.department_id IN (SELECT department_id FROM member_departments)
                       OR faculty.department_id IN (SELECT department_id FROM member_departments)
                   ))
               )
           )
           SELECT jsonb_build_object(
               'scope', $5::text,
               'canManage', $6::boolean,
               'academicYears', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', year.id, 'code', year.code, 'name', year.name,
                       'status', year.status
                   ) ORDER BY year.starts_on DESC)
                   FROM core.academic_years year, tenant
                   WHERE year.tenant_id = tenant.id AND $6
               ), '[]'::jsonb),
               'terms', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', term.id, 'academicYearId', term.academic_year_id,
                       'code', term.code, 'name', term.name,
                       'sequence', term.sequence, 'status', term.status
                   ) ORDER BY term.sequence)
                   FROM core.terms term, tenant
                   WHERE term.tenant_id = tenant.id AND $6
               ), '[]'::jsonb),
               'departments', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', department.id, 'code', department.code,
                       'name', department.name
                   ) ORDER BY department.code)
                   FROM core.departments department, tenant
                   WHERE department.tenant_id = tenant.id AND department.active AND $6
               ), '[]'::jsonb),
               'sections', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', section.id, 'code', section.code, 'name', section.name,
                       'departmentId', programme.department_id,
                       'programmeName', programme.name, 'batchName', batch.name,
                       'capacity', section.capacity
                   ) ORDER BY programme.name, batch.name, section.code)
                   FROM core.sections section
                   JOIN core.batches batch ON batch.id = section.batch_id
                   JOIN core.programmes programme ON programme.id = batch.programme_id
                   JOIN tenant ON tenant.id = section.tenant_id
                   WHERE section.active AND batch.active AND programme.active AND $6
               ), '[]'::jsonb),
               'subjectOfferings', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', offering.id, 'subjectId', subject.id,
                       'code', subject.code, 'name', subject.name,
                       'academicYearId', offering.academic_year_id,
                       'termId', offering.term_id, 'sectionId', offering.section_id,
                       'sectionName', section.name,
                       'departmentId', subject.department_id
                   ) ORDER BY subject.code, section.name)
                   FROM core.subject_offerings offering
                   JOIN core.subjects subject ON subject.id = offering.subject_id
                   JOIN core.sections section ON section.id = offering.section_id
                   JOIN tenant ON tenant.id = offering.tenant_id
                   WHERE offering.active AND subject.active AND $6
               ), '[]'::jsonb),
               'teachingAssignments', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', assignment.id,
                       'subjectOfferingId', assignment.subject_offering_id,
                       'facultyUserId', assignment.faculty_user_id,
                       'facultyName', faculty.display_name,
                       'assignmentType', assignment.assignment_type
                   ) ORDER BY faculty.display_name)
                   FROM core.teaching_assignments assignment
                   JOIN identity.users faculty ON faculty.id = assignment.faculty_user_id
                   JOIN tenant ON tenant.id = assignment.tenant_id
                   WHERE assignment.active AND $6
               ), '[]'::jsonb),
               'latestRevision', COALESCE((
                   SELECT max(event.revision) FROM core.timetable_events event, tenant
                   WHERE event.tenant_id = tenant.id
               ), 0),
               'configurations', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', configuration.id, 'academicYearId', configuration.academic_year_id,
                       'termId', configuration.term_id, 'name', configuration.name,
                       'timezone', configuration.timezone, 'workingDays', configuration.working_days,
                       'maxFacultyPeriodsPerDay', configuration.max_faculty_periods_per_day,
                       'maxConsecutiveFacultyPeriods', configuration.max_consecutive_faculty_periods,
                       'rules', configuration.rules
                   ) ORDER BY configuration.created_at DESC)
                   FROM core.timetable_configurations configuration, tenant
                   WHERE configuration.tenant_id = tenant.id AND configuration.active
                     AND ($6 OR EXISTS (
                         SELECT 1 FROM core.timetable_versions version
                         WHERE version.configuration_id = configuration.id AND version.status = 'published'
                     ))
               ), '[]'::jsonb),
               'slots', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', slot.id, 'configurationId', slot.configuration_id,
                       'dayOfWeek', slot.day_of_week, 'sequence', slot.sequence,
                       'label', slot.label, 'slotType', slot.slot_type,
                       'startsAt', slot.starts_at, 'endsAt', slot.ends_at
                   ) ORDER BY slot.day_of_week, slot.sequence)
                   FROM core.timetable_slots slot, tenant
                   WHERE slot.tenant_id = tenant.id AND ($6 OR EXISTS (
                       SELECT 1 FROM core.timetable_entries entry
                       JOIN visible_entries visible ON visible.id = entry.id
                       WHERE entry.slot_id = slot.id
                   ))
               ), '[]'::jsonb),
               'rooms', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', room.id, 'campusId', room.campus_id,
                       'departmentId', room.department_id, 'code', room.code,
                       'name', room.name, 'roomType', room.room_type,
                       'capacity', room.capacity, 'features', room.features
                   ) ORDER BY room.code)
                   FROM core.rooms room, tenant
                   WHERE room.tenant_id = tenant.id AND room.active
                     AND ($6 OR EXISTS (
                         SELECT 1 FROM core.timetable_entries entry
                         JOIN visible_entries visible ON visible.id = entry.id
                         WHERE entry.room_id = room.id
                     ))
               ), '[]'::jsonb),
               'workloadRequirements', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', requirement.id,
                       'subjectOfferingId', requirement.subject_offering_id,
                       'deliveryType', requirement.delivery_type,
                       'periodsPerWeek', requirement.periods_per_week,
                       'blockSize', requirement.block_size,
                       'maxBlocksPerDay', requirement.max_blocks_per_day,
                       'requiredRoomTypes', requirement.required_room_types,
                       'metadata', requirement.metadata
                   ) ORDER BY requirement.subject_offering_id, requirement.delivery_type)
                   FROM core.subject_offering_workload_requirements requirement, tenant
                   WHERE requirement.tenant_id = tenant.id AND ($6 OR EXISTS (
                       SELECT 1 FROM core.timetable_entries entry
                       JOIN visible_entries visible ON visible.id = entry.id
                       WHERE entry.subject_offering_id = requirement.subject_offering_id
                   ))
               ), '[]'::jsonb),
               'electiveGroups', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', elective.id, 'academicYearId', elective.academic_year_id,
                       'termId', elective.term_id, 'code', elective.code, 'name', elective.name,
                       'sectionIds', COALESCE((SELECT jsonb_agg(link.section_id) FROM core.elective_group_sections link WHERE link.elective_group_id = elective.id), '[]'::jsonb),
                       'studentCount', (SELECT count(*) FROM core.elective_group_students member WHERE member.elective_group_id = elective.id)
                   ) ORDER BY elective.name)
                   FROM core.elective_groups elective, tenant
                   WHERE elective.tenant_id = tenant.id AND elective.active AND ($6 OR EXISTS (
                       SELECT 1 FROM core.timetable_entries entry
                       JOIN visible_entries visible ON visible.id = entry.id
                       WHERE entry.elective_group_id = elective.id
                   ))
               ), '[]'::jsonb),
               'versions', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', version.id, 'configurationId', version.configuration_id,
                       'versionNumber', version.version_number, 'label', version.label,
                       'status', version.status, 'publishedAt', version.published_at
                   ) ORDER BY version.version_number DESC)
                   FROM core.timetable_versions version, tenant
                   WHERE version.tenant_id = tenant.id AND ($6 OR version.status = 'published')
               ), '[]'::jsonb),
               'entries', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', entry.id, 'versionId', entry.version_id, 'slotId', entry.slot_id,
                       'subjectOfferingId', entry.subject_offering_id,
                       'subjectCode', subject.code, 'subjectName', subject.name,
                       'sectionId', offering.section_id, 'sectionName', section.name,
                       'teachingAssignmentId', entry.teaching_assignment_id,
                       'facultyUserId', teaching.faculty_user_id,
                       'facultyName', faculty_user.display_name,
                       'roomId', entry.room_id, 'roomCode', room.code,
                       'electiveGroupId', entry.elective_group_id,
                       'deliveryType', entry.delivery_type,
                       'sessionBlockId', entry.session_block_id,
                       'blockSequence', entry.block_sequence,
                       'blockLength', entry.block_length
                   ) ORDER BY slot.day_of_week, slot.sequence, subject.code)
                   FROM core.timetable_entries entry
                   JOIN visible_entries visible ON visible.id = entry.id
                   JOIN core.timetable_slots slot ON slot.id = entry.slot_id
                   JOIN core.subject_offerings offering ON offering.id = entry.subject_offering_id
                   JOIN core.subjects subject ON subject.id = offering.subject_id
                   JOIN core.sections section ON section.id = offering.section_id
                   JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
                   JOIN identity.users faculty_user ON faculty_user.id = teaching.faculty_user_id
                   JOIN core.rooms room ON room.id = entry.room_id
               ), '[]'::jsonb),
               'substitutions', COALESCE((
                   SELECT jsonb_agg(jsonb_build_object(
                       'id', substitution.id, 'timetableEntryId', substitution.timetable_entry_id,
                       'serviceDate', substitution.service_date,
                       'originalFacultyUserId', substitution.original_faculty_user_id,
                       'substituteFacultyUserId', substitution.substitute_faculty_user_id,
                       'reason', substitution.reason, 'status', substitution.status,
                       'acknowledgements', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                           'facultyUserId', acknowledgement.faculty_user_id,
                           'party', acknowledgement.party,
                           'acknowledgedAt', acknowledgement.acknowledged_at
                       )) FROM core.faculty_substitution_acknowledgements acknowledgement
                       WHERE acknowledgement.request_id = substitution.id), '[]'::jsonb)
                   ) ORDER BY substitution.service_date DESC)
                   FROM core.faculty_substitution_requests substitution
                   JOIN visible_entries visible ON visible.id = substitution.timetable_entry_id
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
    .context("failed to resolve timetable context")?;

    Ok(Json(ApiResponse::new(value)))
}

async fn create_configuration(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateConfigurationRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_timetable_manager(&principal, &access)?;
    if request.name.trim().is_empty()
        || request.timezone.trim().is_empty()
        || request.working_days.is_empty()
        || request
            .working_days
            .iter()
            .any(|day| !(1..=7).contains(day))
        || !(1..=24).contains(&request.max_faculty_periods_per_day)
        || !(1..=24).contains(&request.max_consecutive_faculty_periods)
    {
        return Err(ApiError::BadRequest(
            "invalid timetable configuration".into(),
        ));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let value = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO core.timetable_configurations
               (tenant_id, academic_year_id, term_id, name, timezone, working_days,
                max_faculty_periods_per_day, max_consecutive_faculty_periods, rules, created_by)
           SELECT tenant.id, year.id, term.id, $4, $5, $6, $7, $8,
                  jsonb_build_object('preset', 'anna-university-2025',
                     'enforceRoomCapacity', true, 'allowCrossSectionElectives', true,
                     'requiredSectionPeriodsPerWeek', 35) || $9, $10
           FROM platform.tenants tenant
           JOIN core.academic_years year ON year.tenant_id = tenant.id AND year.id = $2
           LEFT JOIN core.terms term ON term.tenant_id = tenant.id AND term.id = $3
           WHERE tenant.slug = $1 AND ($3 IS NULL OR term.id IS NOT NULL)
           RETURNING to_jsonb(core.timetable_configurations.*)"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.academic_year_id)
    .bind(request.term_id)
    .bind(request.name.trim())
    .bind(request.timezone.trim())
    .bind(&request.working_days)
    .bind(request.max_faculty_periods_per_day)
    .bind(request.max_consecutive_faculty_periods)
    .bind(request.rules)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::BadRequest("academic year or term does not belong to this tenant".into())
    })?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "configuration",
        id,
        "timetable.configuration.created",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn replace_slots(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(configuration_id): Path<Uuid>,
    Json(request): Json<ReplaceSlotsRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_timetable_manager(&principal, &access)?;
    if request.slots.is_empty() {
        return Err(ApiError::BadRequest("at least one slot is required".into()));
    }
    for slot in &request.slots {
        if !(1..=7).contains(&slot.day_of_week)
            || slot.sequence < 1
            || slot.label.trim().is_empty()
            || !matches!(slot.slot_type.as_str(), "instructional" | "break" | "lunch")
            || slot.ends_at <= slot.starts_at
        {
            return Err(ApiError::BadRequest(
                "one or more timetable slots are invalid".into(),
            ));
        }
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let editable = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM core.timetable_configurations configuration
           JOIN platform.tenants tenant ON tenant.id = configuration.tenant_id
           WHERE tenant.slug = $1 AND configuration.id = $2 AND configuration.active
             AND NOT EXISTS (SELECT 1 FROM core.timetable_versions version
                             WHERE version.configuration_id = configuration.id AND version.status = 'published'))"#)
        .bind(&principal.student.tenant_id).bind(configuration_id).fetch_one(&mut *tx).await?;
    if !editable {
        return Err(ApiError::Conflict(
            "published timetable slots cannot be replaced; create a new configuration".into(),
        ));
    }
    sqlx::query("DELETE FROM core.timetable_slots slot USING platform.tenants tenant WHERE tenant.id = slot.tenant_id AND tenant.slug = $1 AND slot.configuration_id = $2")
        .bind(&principal.student.tenant_id).bind(configuration_id).execute(&mut *tx).await?;
    for slot in &request.slots {
        sqlx::query(r#"INSERT INTO core.timetable_slots
            (tenant_id, configuration_id, day_of_week, sequence, label, slot_type, starts_at, ends_at)
            SELECT tenant.id, $2, $3, $4, $5, $6, $7, $8 FROM platform.tenants tenant WHERE tenant.slug = $1"#)
            .bind(&principal.student.tenant_id).bind(configuration_id).bind(slot.day_of_week)
            .bind(slot.sequence).bind(slot.label.trim()).bind(&slot.slot_type)
            .bind(slot.starts_at).bind(slot.ends_at).execute(&mut *tx).await?;
    }
    let payload = json!({"configurationId": configuration_id, "slotCount": request.slots.len()});
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "configuration",
        configuration_id,
        "timetable.slots.replaced",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(payload)))
}

async fn create_room(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateRoomRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_timetable_manager(&principal, &access)?;
    if request.code.trim().is_empty()
        || request.name.trim().is_empty()
        || request.capacity < 1
        || !valid_room_type(&request.room_type)
        || (request.department_id.is_some() && request.department_code.is_some())
        || request
            .department_code
            .as_deref()
            .is_some_and(|code| code.trim().is_empty())
    {
        return Err(ApiError::BadRequest("invalid room".into()));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let value = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO core.rooms
        (tenant_id, campus_id, department_id, code, name, room_type, capacity, features, created_by)
        SELECT tenant.id, campus.id, department.id, $5, $6, $7, $8, $9, $10
        FROM platform.tenants tenant
        LEFT JOIN core.campuses campus ON campus.tenant_id = tenant.id AND campus.id = $2
        LEFT JOIN core.departments department ON department.tenant_id = tenant.id
          AND (($3::uuid IS NOT NULL AND department.id = $3)
            OR ($3 IS NULL AND NULLIF(BTRIM($4), '') IS NOT NULL
              AND UPPER(department.code) = UPPER(BTRIM($4))))
        WHERE tenant.slug = $1 AND ($2 IS NULL OR campus.id IS NOT NULL)
          AND (($3 IS NULL AND NULLIF(BTRIM($4), '') IS NULL) OR department.id IS NOT NULL)
        RETURNING to_jsonb(core.rooms.*)"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.campus_id)
    .bind(request.department_id)
    .bind(request.department_code.as_deref())
    .bind(request.code.trim())
    .bind(request.name.trim())
    .bind(&request.room_type)
    .bind(request.capacity)
    .bind(request.features)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::BadRequest("campus or department does not belong to this tenant".into())
    })?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "room",
        id,
        "timetable.room.created",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn create_rooms_bulk(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateRoomsBulkRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_timetable_manager(&principal, &access)?;
    if request.rooms.is_empty() || request.rooms.len() > 250 {
        return Err(ApiError::BadRequest(
            "provide between 1 and 250 rooms".into(),
        ));
    }
    if request.rooms.iter().any(|room| {
        room.code.trim().is_empty()
            || room.name.trim().is_empty()
            || room.capacity < 1
            || !valid_room_type(&room.room_type)
            || (room.department_id.is_some() && room.department_code.is_some())
            || room
                .department_code
                .as_deref()
                .is_some_and(|code| code.trim().is_empty())
    }) {
        return Err(ApiError::BadRequest("one or more rooms are invalid".into()));
    }

    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let mut rooms = Vec::with_capacity(request.rooms.len());
    for room in request.rooms {
        let value = sqlx::query_scalar::<_, Value>(r#"INSERT INTO core.rooms
            (tenant_id, campus_id, department_id, code, name, room_type, capacity, features, created_by)
            SELECT tenant.id, campus.id, department.id, $5, $6, $7, $8, $9, $10
            FROM platform.tenants tenant
            LEFT JOIN core.campuses campus ON campus.tenant_id = tenant.id AND campus.id = $2
            LEFT JOIN core.departments department ON department.tenant_id = tenant.id
              AND (($3::uuid IS NOT NULL AND department.id = $3)
                OR ($3 IS NULL AND NULLIF(BTRIM($4), '') IS NOT NULL
                  AND UPPER(department.code) = UPPER(BTRIM($4))))
            WHERE tenant.slug = $1 AND ($2 IS NULL OR campus.id IS NOT NULL)
              AND (($3 IS NULL AND NULLIF(BTRIM($4), '') IS NULL) OR department.id IS NOT NULL)
            ON CONFLICT (tenant_id, code) DO UPDATE SET
                campus_id = EXCLUDED.campus_id, department_id = EXCLUDED.department_id,
                name = EXCLUDED.name, room_type = EXCLUDED.room_type,
                capacity = EXCLUDED.capacity, features = EXCLUDED.features,
                active = true, updated_at = now()
            RETURNING to_jsonb(core.rooms.*)"#)
            .bind(&principal.student.tenant_id).bind(room.campus_id).bind(room.department_id)
            .bind(room.department_code.as_deref()).bind(room.code.trim()).bind(room.name.trim()).bind(&room.room_type)
            .bind(room.capacity).bind(room.features).bind(actor)
            .fetch_optional(&mut *tx).await?
            .ok_or_else(|| ApiError::BadRequest("campus or department does not belong to this tenant".into()))?;
        rooms.push(value);
    }
    let payload = json!({"rooms": rooms, "count": rooms.len()});
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "room_inventory",
        Uuid::new_v4(),
        "timetable.rooms.bulk_upserted",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(payload)))
}

async fn upsert_workload_requirement(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<UpsertWorkloadRequirementRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_timetable_manager(&principal, &access)?;
    if !valid_delivery_type(&request.delivery_type)
        || !(1..=35).contains(&request.periods_per_week)
        || !(1..=7).contains(&request.block_size)
        || !(1..=7).contains(&request.max_blocks_per_day)
        || request
            .required_room_types
            .iter()
            .any(|kind| !valid_room_type(kind))
    {
        return Err(ApiError::BadRequest("invalid workload requirement".into()));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let value = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO core.subject_offering_workload_requirements
        (tenant_id, subject_offering_id, delivery_type, periods_per_week, block_size,
         max_blocks_per_day, required_room_types, metadata, created_by)
        SELECT tenant.id, offering.id, $3, $4, $5, $6, $7, $8, $9
        FROM platform.tenants tenant
        JOIN core.subject_offerings offering ON offering.tenant_id = tenant.id
             AND offering.id = $2 AND offering.active
        WHERE tenant.slug = $1
        ON CONFLICT (tenant_id, subject_offering_id, delivery_type) DO UPDATE SET
            periods_per_week = EXCLUDED.periods_per_week,
            block_size = EXCLUDED.block_size,
            max_blocks_per_day = EXCLUDED.max_blocks_per_day,
            required_room_types = EXCLUDED.required_room_types,
            metadata = EXCLUDED.metadata,
            updated_at = now()
        RETURNING to_jsonb(core.subject_offering_workload_requirements.*)"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.subject_offering_id)
    .bind(&request.delivery_type)
    .bind(request.periods_per_week)
    .bind(request.block_size)
    .bind(request.max_blocks_per_day)
    .bind(&request.required_room_types)
    .bind(request.metadata)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::BadRequest("subject offering does not belong to this tenant".into())
    })?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "workload_requirement",
        id,
        "timetable.workload_requirement.upserted",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(value)))
}

async fn create_elective_group(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateElectiveGroupRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_timetable_manager(&principal, &access)?;
    if request.code.trim().is_empty()
        || request.name.trim().is_empty()
        || request.section_ids.is_empty()
    {
        return Err(ApiError::BadRequest(
            "elective code, name, and at least one section are required".into(),
        ));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO core.elective_groups
        (tenant_id, academic_year_id, term_id, code, name, created_by)
        SELECT tenant.id, year.id, term.id, $4, $5, $6 FROM platform.tenants tenant
        JOIN core.academic_years year ON year.tenant_id = tenant.id AND year.id = $2
        LEFT JOIN core.terms term ON term.tenant_id = tenant.id AND term.id = $3
        WHERE tenant.slug = $1 AND ($3 IS NULL OR term.id IS NOT NULL) RETURNING id"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request.academic_year_id)
    .bind(request.term_id)
    .bind(request.code.trim())
    .bind(request.name.trim())
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::BadRequest("academic year or term does not belong to this tenant".into())
    })?;
    let linked_sections = sqlx::query(
        r#"INSERT INTO core.elective_group_sections (tenant_id, elective_group_id, section_id)
        SELECT tenant.id, $2, section.id FROM platform.tenants tenant
        JOIN core.sections section ON section.tenant_id = tenant.id AND section.id = ANY($3)
        WHERE tenant.slug = $1 ON CONFLICT DO NOTHING"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(id)
    .bind(&request.section_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if linked_sections != request.section_ids.len() as u64 {
        return Err(ApiError::BadRequest(
            "one or more sections do not belong to this tenant".into(),
        ));
    }
    if !request.student_ids.is_empty() {
        let linked_students = sqlx::query(
            r#"INSERT INTO core.elective_group_students (tenant_id, elective_group_id, student_id)
            SELECT tenant.id, $2, student.id FROM platform.tenants tenant
            JOIN core.students student ON student.tenant_id = tenant.id AND student.id = ANY($3)
            WHERE tenant.slug = $1 ON CONFLICT DO NOTHING"#,
        )
        .bind(&principal.student.tenant_id)
        .bind(id)
        .bind(&request.student_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if linked_students != request.student_ids.len() as u64 {
            return Err(ApiError::BadRequest(
                "one or more students do not belong to this tenant".into(),
            ));
        }
    }
    let payload =
        json!({"id": id, "sectionIds": request.section_ids, "studentIds": request.student_ids});
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "elective_group",
        id,
        "timetable.elective_group.created",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(payload))))
}

async fn create_version(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateVersionRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_timetable_manager(&principal, &access)?;
    if request.label.trim().is_empty() {
        return Err(ApiError::BadRequest("version label is required".into()));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let value = sqlx::query_scalar::<_, Value>(r#"INSERT INTO core.timetable_versions
        (tenant_id, configuration_id, version_number, label, rules_snapshot, created_by)
        SELECT tenant.id, configuration.id,
               COALESCE((SELECT max(existing.version_number) + 1 FROM core.timetable_versions existing WHERE existing.configuration_id = configuration.id), 1),
               $3, configuration.rules || jsonb_build_object('workingDays', configuration.working_days,
               'maxFacultyPeriodsPerDay', configuration.max_faculty_periods_per_day,
               'maxConsecutiveFacultyPeriods', configuration.max_consecutive_faculty_periods), $4
        FROM platform.tenants tenant JOIN core.timetable_configurations configuration
          ON configuration.tenant_id = tenant.id AND configuration.id = $2 AND configuration.active
        WHERE tenant.slug = $1 RETURNING to_jsonb(core.timetable_versions.*)"#)
        .bind(&principal.student.tenant_id).bind(request.configuration_id).bind(request.label.trim()).bind(actor)
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::BadRequest("configuration does not belong to this tenant".into()))?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "version",
        id,
        "timetable.version.created",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn create_entry(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateEntryRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_timetable_manager(&principal, &access)?;
    if !valid_delivery_type(&request.delivery_type)
        || !(1..=7).contains(&request.block_length)
        || !(1..=request.block_length).contains(&request.block_sequence)
    {
        return Err(ApiError::BadRequest(
            "invalid delivery type or session block position".into(),
        ));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let session_block_id = request.session_block_id.unwrap_or_else(Uuid::new_v4);
    let value = sqlx::query_scalar::<_, Value>(r#"INSERT INTO core.timetable_entries
        (tenant_id, version_id, slot_id, subject_offering_id, teaching_assignment_id,
         room_id, elective_group_id, delivery_type, session_block_id, block_sequence,
         block_length, created_by)
        SELECT tenant.id, version.id, slot.id, offering.id, teaching.id, room.id, elective.id,
               $8, $9, $10, $11, $12
        FROM platform.tenants tenant
        JOIN core.timetable_versions version ON version.tenant_id = tenant.id AND version.id = $2 AND version.status = 'draft'
        JOIN core.timetable_slots slot ON slot.tenant_id = tenant.id AND slot.id = $3
             AND slot.configuration_id = version.configuration_id AND slot.slot_type = 'instructional'
        JOIN core.subject_offerings offering ON offering.tenant_id = tenant.id AND offering.id = $4 AND offering.active
        JOIN core.teaching_assignments teaching ON teaching.tenant_id = tenant.id AND teaching.id = $5
             AND teaching.subject_offering_id = offering.id AND teaching.active
        JOIN core.rooms room ON room.tenant_id = tenant.id AND room.id = $6 AND room.active
        LEFT JOIN core.elective_groups elective ON elective.tenant_id = tenant.id AND elective.id = $7 AND elective.active
        WHERE tenant.slug = $1 AND ($7 IS NULL OR elective.id IS NOT NULL)
        RETURNING to_jsonb(core.timetable_entries.*)"#)
        .bind(&principal.student.tenant_id).bind(request.version_id).bind(request.slot_id)
        .bind(request.subject_offering_id).bind(request.teaching_assignment_id).bind(request.room_id)
        .bind(request.elective_group_id).bind(&request.delivery_type).bind(session_block_id)
        .bind(request.block_sequence).bind(request.block_length).bind(actor)
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::BadRequest("entry relationships are invalid or the version is not a draft".into()))?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "entry",
        id,
        "timetable.entry.created",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn publish_version(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(version_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_timetable_manager(&principal, &access)?;
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let conflicts =
        publication_conflicts(&mut tx, &principal.student.tenant_id, version_id).await?;
    if conflicts.as_array().is_some_and(|items| !items.is_empty()) {
        return Err(ApiError::Conflict(format!(
            "timetable has conflicts: {conflicts}"
        )));
    }
    let configuration_id = sqlx::query_scalar::<_, Uuid>(r#"SELECT version.configuration_id
        FROM core.timetable_versions version JOIN platform.tenants tenant ON tenant.id = version.tenant_id
        WHERE tenant.slug = $1 AND version.id = $2 AND version.status = 'draft' FOR UPDATE"#)
        .bind(&principal.student.tenant_id).bind(version_id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::Conflict("only a draft timetable can be published".into()))?;
    sqlx::query("UPDATE core.timetable_versions SET status = 'superseded', updated_at = now() WHERE tenant_id = (SELECT id FROM platform.tenants WHERE slug = $1) AND configuration_id = $2 AND status = 'published'")
        .bind(&principal.student.tenant_id).bind(configuration_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE core.timetable_versions SET status = 'published', published_by = $3, published_at = now(), updated_at = now() WHERE tenant_id = (SELECT id FROM platform.tenants WHERE slug = $1) AND id = $2")
        .bind(&principal.student.tenant_id).bind(version_id).bind(actor).execute(&mut *tx).await?;
    let payload = json!({"versionId": version_id, "configurationId": configuration_id, "status": "published"});
    let revision = emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "version",
        version_id,
        "timetable.version.published",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(
        json!({"revision": revision, "version": payload}),
    )))
}

async fn publication_conflicts(
    tx: &mut Transaction<'_, Postgres>,
    tenant_slug: &str,
    version_id: Uuid,
) -> ApiResult<Value> {
    let value = sqlx::query_scalar::<_, Value>(r#"WITH target AS (
        SELECT version.id AS version_id, version.configuration_id, configuration.academic_year_id,
               configuration.term_id, configuration.rules, tenant.id AS tenant_id
        FROM core.timetable_versions version
        JOIN core.timetable_configurations configuration ON configuration.id = version.configuration_id
        JOIN platform.tenants tenant ON tenant.id = version.tenant_id
        WHERE tenant.slug = $1 AND version.id = $2
    ), entries AS (
        SELECT entry.id, entry.room_id, entry.subject_offering_id, entry.teaching_assignment_id,
               slot.day_of_week, slot.sequence, offering.section_id, teaching.faculty_user_id,
               room.capacity, room.room_type, section.capacity AS section_capacity,
               entry.delivery_type, entry.session_block_id, entry.block_sequence, entry.block_length,
               configuration.max_faculty_periods_per_day,
               configuration.max_consecutive_faculty_periods
        FROM core.timetable_entries entry
        JOIN target ON target.tenant_id = entry.tenant_id AND target.version_id = entry.version_id
        JOIN core.timetable_versions version ON version.id = entry.version_id
        JOIN core.timetable_configurations configuration ON configuration.id = version.configuration_id
        JOIN core.timetable_slots slot ON slot.id = entry.slot_id
        JOIN core.subject_offerings offering ON offering.id = entry.subject_offering_id
        JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
        JOIN core.rooms room ON room.id = entry.room_id
        JOIN core.sections section ON section.id = offering.section_id
    ), requirements AS (
        SELECT requirement.*, offering.section_id
        FROM core.subject_offering_workload_requirements requirement
        JOIN target ON target.tenant_id = requirement.tenant_id
        JOIN core.subject_offerings offering ON offering.id = requirement.subject_offering_id
             AND offering.academic_year_id = target.academic_year_id
             AND offering.term_id IS NOT DISTINCT FROM target.term_id
        WHERE offering.active
    ), actual_workloads AS (
        SELECT subject_offering_id, delivery_type, count(*)::integer AS periods
        FROM entries GROUP BY subject_offering_id, delivery_type
    ), session_blocks AS (
        SELECT session_block_id, subject_offering_id, delivery_type,
               min(day_of_week) AS day_of_week, min(sequence) AS sequence,
               min(block_length) AS declared_length, count(*)::integer AS actual_length,
               min(sequence) AS first_sequence, max(sequence) AS last_sequence,
               count(DISTINCT day_of_week) AS day_count,
               count(DISTINCT room_id) AS room_count,
               count(DISTINCT teaching_assignment_id) AS teaching_count,
               count(DISTINCT block_sequence) AS block_sequence_count
        FROM entries
        GROUP BY session_block_id, subject_offering_id, delivery_type
    ), daily_blocks AS (
        SELECT block.subject_offering_id, block.delivery_type, block.day_of_week,
               count(*)::integer AS block_count, min(block.sequence) AS sequence
        FROM session_blocks block
        GROUP BY block.subject_offering_id, block.delivery_type, block.day_of_week
    ), section_requirement_totals AS (
        SELECT requirement.section_id, sum(requirement.periods_per_week)::integer AS periods,
               COALESCE((target.rules ->> 'requiredSectionPeriodsPerWeek')::integer, 35) AS expected
        FROM requirements requirement CROSS JOIN target
        GROUP BY requirement.section_id, target.rules
    ), faculty_sequences AS (
        SELECT faculty_user_id, day_of_week, sequence, max_consecutive_faculty_periods,
               sequence - row_number() OVER (
                   PARTITION BY faculty_user_id, day_of_week ORDER BY sequence
               )::integer AS sequence_group
        FROM entries
    ), faculty_streaks AS (
        SELECT faculty_user_id, day_of_week, min(sequence) AS sequence
        FROM faculty_sequences
        GROUP BY faculty_user_id, day_of_week, sequence_group, max_consecutive_faculty_periods
        HAVING count(*) > max_consecutive_faculty_periods
    ), conflicts AS (
        SELECT 'room' AS kind, room_id::text AS resource, day_of_week, sequence FROM entries
        GROUP BY room_id, day_of_week, sequence HAVING count(*) > 1
        UNION ALL
        SELECT 'faculty', faculty_user_id::text, day_of_week, sequence FROM entries
        GROUP BY faculty_user_id, day_of_week, sequence HAVING count(*) > 1
        UNION ALL
        SELECT 'section', section_id::text, day_of_week, sequence FROM entries
        GROUP BY section_id, day_of_week, sequence HAVING count(*) > 1
        UNION ALL
        SELECT 'room_capacity', room_id::text, day_of_week, sequence FROM entries
        WHERE section_capacity IS NOT NULL AND section_capacity > capacity
        UNION ALL
        SELECT 'faculty_daily_limit', faculty_user_id::text, day_of_week, 0 FROM entries
        GROUP BY faculty_user_id, day_of_week, max_faculty_periods_per_day
        HAVING count(*) > max_faculty_periods_per_day
        UNION ALL
        SELECT 'faculty_consecutive_limit', faculty_user_id::text, day_of_week, sequence
        FROM faculty_streaks
        UNION ALL
        SELECT 'workload_periods', requirement.subject_offering_id::text || ':' || requirement.delivery_type,
               0, 0
        FROM requirements requirement
        LEFT JOIN actual_workloads actual
          ON actual.subject_offering_id = requirement.subject_offering_id
         AND actual.delivery_type = requirement.delivery_type
        WHERE COALESCE(actual.periods, 0) <> requirement.periods_per_week
        UNION ALL
        SELECT 'workload_not_configured', actual.subject_offering_id::text || ':' || actual.delivery_type,
               0, 0
        FROM actual_workloads actual
        LEFT JOIN requirements requirement
          ON requirement.subject_offering_id = actual.subject_offering_id
         AND requirement.delivery_type = actual.delivery_type
        WHERE requirement.id IS NULL
        UNION ALL
        SELECT 'session_block', block.session_block_id::text, block.day_of_week, block.sequence
        FROM session_blocks block
        JOIN requirements requirement
          ON requirement.subject_offering_id = block.subject_offering_id
         AND requirement.delivery_type = block.delivery_type
        WHERE block.actual_length <> requirement.block_size
           OR block.declared_length <> requirement.block_size
           OR block.last_sequence - block.first_sequence + 1 <> block.actual_length
           OR block.day_count <> 1 OR block.room_count <> 1 OR block.teaching_count <> 1
           OR block.block_sequence_count <> block.actual_length
           OR NOT EXISTS (
               SELECT 1 FROM entries member
               WHERE member.session_block_id = block.session_block_id
                 AND member.block_sequence = 1
           )
           OR NOT EXISTS (
               SELECT 1 FROM entries member
               WHERE member.session_block_id = block.session_block_id
                 AND member.block_sequence = requirement.block_size
           )
        UNION ALL
        SELECT 'room_type', entry.room_id::text, entry.day_of_week, entry.sequence
        FROM entries entry
        JOIN requirements requirement
          ON requirement.subject_offering_id = entry.subject_offering_id
         AND requirement.delivery_type = entry.delivery_type
        WHERE cardinality(requirement.required_room_types) > 0
          AND NOT (entry.room_type = ANY(requirement.required_room_types))
        UNION ALL
        SELECT 'daily_block_limit', daily.subject_offering_id::text || ':' || daily.delivery_type,
               daily.day_of_week, daily.sequence
        FROM daily_blocks daily
        JOIN requirements requirement
          ON requirement.subject_offering_id = daily.subject_offering_id
         AND requirement.delivery_type = daily.delivery_type
        WHERE daily.block_count > requirement.max_blocks_per_day
        UNION ALL
        SELECT 'section_weekly_periods', total.section_id::text, 0, 0
        FROM section_requirement_totals total WHERE total.periods <> total.expected
    ) SELECT COALESCE(jsonb_agg(jsonb_build_object(
        'type', kind, 'resourceId', resource, 'dayOfWeek', day_of_week, 'sequence', sequence
    )), '[]'::jsonb) FROM conflicts"#)
        .bind(tenant_slug).bind(version_id).fetch_one(&mut **tx).await?;
    Ok(value)
}

async fn request_substitution(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<RequestSubstitutionRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    if !access.allows("academics.timetable.substitution.request") {
        return Err(ApiError::Forbidden);
    }
    if request.reason.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "substitution reason is required".into(),
        ));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let value = sqlx::query_scalar::<_, Value>(r#"INSERT INTO core.faculty_substitution_requests
        (tenant_id, timetable_entry_id, service_date, original_faculty_user_id,
         substitute_faculty_user_id, reason, requested_by)
        SELECT tenant.id, entry.id, $3, teaching.faculty_user_id, substitute.id, $5, $6
        FROM platform.tenants tenant
        JOIN core.timetable_entries entry ON entry.tenant_id = tenant.id AND entry.id = $2
        JOIN core.timetable_versions version ON version.id = entry.version_id AND version.status = 'published'
        JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
        JOIN identity.tenant_memberships membership ON membership.tenant_id = tenant.id
             AND membership.user_id = $4 AND membership.active AND 'faculty' = ANY(membership.roles)
        JOIN identity.users substitute ON substitute.id = membership.user_id AND substitute.active
        WHERE tenant.slug = $1 AND ($6 = teaching.faculty_user_id OR $7)
        RETURNING to_jsonb(core.faculty_substitution_requests.*)"#)
        .bind(&principal.student.tenant_id).bind(request.timetable_entry_id).bind(request.service_date)
        .bind(request.substitute_faculty_user_id).bind(request.reason.trim()).bind(actor)
        .bind(timetable_manager_allowed(&principal, &access))
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::BadRequest("published class, substitute Faculty, or request authority is invalid".into()))?;
    let id = json_uuid(&value, "id")?;
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "substitution",
        id,
        "timetable.substitution.requested",
        actor,
        value.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn acknowledge_substitution(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<Uuid>,
    Json(request): Json<AcknowledgeSubstitutionRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !access.allows("academics.timetable.substitution.acknowledge") {
        return Err(ApiError::Forbidden);
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let party = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT CASE
        WHEN substitution.original_faculty_user_id = $3 THEN 'original'
        WHEN substitution.substitute_faculty_user_id = $3 THEN 'substitute' END
        FROM core.faculty_substitution_requests substitution
        JOIN platform.tenants tenant ON tenant.id = substitution.tenant_id
        WHERE tenant.slug = $1 AND substitution.id = $2
          AND substitution.status IN ('awaiting_acknowledgements', 'awaiting_principal')"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request_id)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .flatten()
    .ok_or(ApiError::Forbidden)?;
    sqlx::query(r#"INSERT INTO core.faculty_substitution_acknowledgements
        (tenant_id, request_id, faculty_user_id, party, evidence)
        SELECT tenant.id, $2, $3, $4, $5 FROM platform.tenants tenant WHERE tenant.slug = $1
        ON CONFLICT (tenant_id, request_id, party) DO UPDATE SET
          faculty_user_id = EXCLUDED.faculty_user_id, evidence = EXCLUDED.evidence, acknowledged_at = now()"#)
        .bind(&principal.student.tenant_id).bind(request_id).bind(actor).bind(&party).bind(request.evidence)
        .execute(&mut *tx).await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM core.faculty_substitution_acknowledgements WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    if count == 2 {
        sqlx::query("UPDATE core.faculty_substitution_requests SET status = 'awaiting_principal', updated_at = now() WHERE id = $1 AND status = 'awaiting_acknowledgements'")
            .bind(request_id).execute(&mut *tx).await?;
    }
    let payload = json!({"requestId": request_id, "party": party, "acknowledgementCount": count, "status": if count == 2 {"awaiting_principal"} else {"awaiting_acknowledgements"}});
    emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "substitution",
        request_id,
        "timetable.substitution.acknowledged",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(payload)))
}

async fn decide_substitution(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<Uuid>,
    Json(request): Json<DecideSubstitutionRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !access.allows("academics.timetable.substitution.approve")
        || !any_role_may_perform(
            principal.roles.iter().map(String::as_str),
            GovernedCapability::FacultySubstitutionApproval,
        )
    {
        return Err(ApiError::Forbidden);
    }
    if !matches!(request.decision.as_str(), "approved" | "rejected") {
        return Err(ApiError::BadRequest(
            "decision must be approved or rejected".into(),
        ));
    }
    let actor = principal_user_id(&principal)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut tx = database.pool().begin().await?;
    let updated = sqlx::query(
        r#"UPDATE core.faculty_substitution_requests substitution
        SET status = $3, decided_by = $4, decision_note = $5, decided_at = now(), updated_at = now()
        FROM platform.tenants tenant WHERE tenant.id = substitution.tenant_id AND tenant.slug = $1
          AND substitution.id = $2 AND substitution.status = 'awaiting_principal'
          AND (SELECT count(*) FROM core.faculty_substitution_acknowledgements acknowledgement
               WHERE acknowledgement.request_id = substitution.id) = 2"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(request_id)
    .bind(&request.decision)
    .bind(actor)
    .bind(request.note.as_deref().map(str::trim))
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::Conflict(
            "both Faculty acknowledgements are required before the Principal decision".into(),
        ));
    }
    let payload =
        json!({"requestId": request_id, "status": request.decision, "decisionNote": request.note});
    let revision = emit_event(
        &mut tx,
        &principal.student.tenant_id,
        "substitution",
        request_id,
        "timetable.substitution.decided",
        actor,
        payload.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(
        json!({"revision": revision, "substitution": payload}),
    )))
}

async fn changes(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(cursor): Query<ChangeCursor>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_read(&access)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let limit = cursor.limit.clamp(1, 500);
    let value = sqlx::query_scalar::<_, Value>(
        r#"SELECT jsonb_build_object(
        'events', COALESCE(jsonb_agg(jsonb_build_object(
            'revision', event.revision, 'aggregateType', event.aggregate_type,
            'aggregateId', event.aggregate_id, 'eventType', event.event_type,
            'payload', event.payload, 'createdAt', event.created_at
        ) ORDER BY event.revision) FILTER (WHERE event.revision IS NOT NULL), '[]'::jsonb),
        'latestRevision', COALESCE(max(event.revision), $2))
        FROM (SELECT event.* FROM core.timetable_events event
              JOIN platform.tenants tenant ON tenant.id = event.tenant_id
              WHERE tenant.slug = $1 AND event.revision > $2
              ORDER BY event.revision LIMIT $3) event"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(cursor.after.max(0))
    .bind(limit)
    .fetch_one(database.pool())
    .await?;
    Ok(Json(ApiResponse::new(value)))
}

fn require_read(access: &EffectiveAccess) -> ApiResult<()> {
    if access.allows("academics.timetable.read") || access.allows("academics.timetable.manage") {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn valid_delivery_type(value: &str) -> bool {
    matches!(
        value,
        "class" | "laboratory" | "tutorial" | "project" | "activity"
    )
}

fn valid_room_type(value: &str) -> bool {
    matches!(
        value,
        "classroom"
            | "tutorial_room"
            | "laboratory"
            | "computer_lab"
            | "chemistry_lab"
            | "physics_lab"
            | "workshop"
            | "library"
            | "staff_room"
            | "seminar_hall"
            | "auditorium"
            | "sports"
            | "other"
    )
}

fn read_scope(access: &EffectiveAccess) -> &str {
    access
        .scope_for("academics.timetable.read")
        .or_else(|| access.scope_for("academics.timetable.manage"))
        .unwrap_or("assigned")
}

fn timetable_manager_allowed(principal: &AuthPrincipal, access: &EffectiveAccess) -> bool {
    access.allows("academics.timetable.manage")
        && access
            .scope_for("academics.timetable.manage")
            .is_some_and(|scope| matches!(scope, "institution" | "all"))
        && any_role_may_perform(
            principal.roles.iter().map(String::as_str),
            GovernedCapability::TimetableManagement,
        )
}

fn require_timetable_manager(principal: &AuthPrincipal, access: &EffectiveAccess) -> ApiResult<()> {
    if timetable_manager_allowed(principal, access) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn principal_user_id(principal: &AuthPrincipal) -> ApiResult<Uuid> {
    Uuid::parse_str(&principal.student.id).map_err(|_| ApiError::Internal)
}

fn json_uuid(value: &Value, key: &str) -> ApiResult<Uuid> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(ApiError::Internal)
}

#[allow(clippy::too_many_arguments)]
async fn emit_event(
    tx: &mut Transaction<'_, Postgres>,
    tenant_slug: &str,
    aggregate_type: &str,
    aggregate_id: Uuid,
    event_type: &str,
    actor: Uuid,
    payload: Value,
) -> ApiResult<i64> {
    let revision = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO core.timetable_events
        (tenant_id, aggregate_type, aggregate_id, event_type, actor_user_id, payload)
        SELECT tenant.id, $2, $3, $4, $5, $6 FROM platform.tenants tenant WHERE tenant.slug = $1
        RETURNING revision"#,
    )
    .bind(tenant_slug)
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(event_type)
    .bind(actor)
    .bind(payload)
    .fetch_one(&mut **tx)
    .await?;
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthStudent, TenantSummary};
    use std::collections::HashMap;

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
                photo_url: String::new(),
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

    fn access(permission: &str, scope: &str) -> EffectiveAccess {
        EffectiveAccess {
            roles: vec![],
            portal_families: vec!["staff".into()],
            permissions: vec![permission.into()],
            scopes: HashMap::from([(permission.into(), scope.into())]),
        }
    }

    #[test]
    fn timetable_writes_require_permission_scope_and_governed_role() {
        let allowed = access("academics.timetable.manage", "institution");
        assert!(require_timetable_manager(&principal("principal"), &allowed).is_ok());
        assert!(require_timetable_manager(&principal("academic_administrator"), &allowed).is_ok());
        assert!(require_timetable_manager(&principal("hod"), &allowed).is_err());
        assert!(
            require_timetable_manager(
                &principal("principal"),
                &access("academics.timetable.manage", "department")
            )
            .is_err()
        );
    }

    #[test]
    fn mec_space_types_are_supported_without_tenant_hardcoding() {
        for room_type in [
            "classroom",
            "tutorial_room",
            "computer_lab",
            "chemistry_lab",
            "physics_lab",
            "workshop",
            "library",
            "staff_room",
            "seminar_hall",
        ] {
            assert!(valid_room_type(room_type));
        }
        assert!(!valid_room_type("department_room_1"));
    }
}
