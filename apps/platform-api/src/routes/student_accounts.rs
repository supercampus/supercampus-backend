//! Create-only student onboarding. Pending identities cannot sign in until the
//! tenant master and student role have been installed. Retries resume by key.
use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};
use axum::{Extension, Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::HashSet;
use uuid::Uuid;

const MINIMUM_PASSWORD_LENGTH: usize = 8;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    rows: Vec<StudentRow>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StudentRow {
    name: String,
    roll_no: String,
    email: String,
    mobile_number: String,
    department: String,
    year: u8,
    section: String,
    password: String,
}

fn validate(rows: &[StudentRow]) -> Result<(), String> {
    if rows.is_empty() || rows.len() > 25 {
        return Err("Submit 1–25 students per batch".into());
    }
    let mut emails = HashSet::new();
    let mut rolls = HashSet::new();
    for (i, r) in rows.iter().enumerate() {
        let email = r.email.trim().to_ascii_lowercase();
        let invalid = r.name.trim().is_empty()
            || r.name.len() > 200
            || r.roll_no.trim().is_empty()
            || r.roll_no.len() > 64
            || r.department.trim().is_empty()
            || r.department.len() > 100
            || r.section.trim().is_empty()
            || r.section.len() > 40
            || !(1..=6).contains(&r.year)
            || !email.contains('@')
            || email.chars().any(char::is_whitespace)
            || email.len() > 254
            || r.mobile_number.trim().is_empty()
            || r.mobile_number.len() > 20
            || r.password.chars().count() < MINIMUM_PASSWORD_LENGTH
            || r.password.len() > 72;
        if invalid {
            return Err(format!(
                "Row {}: check required fields, year (1–6), email and password (at least {MINIMUM_PASSWORD_LENGTH} characters, at most 72 bytes)",
                i + 2
            ));
        }
        if !emails.insert(email) || !rolls.insert(r.roll_no.trim().to_ascii_lowercase()) {
            return Err(format!("Row {}: duplicate email or roll number", i + 2));
        }
    }
    Ok(())
}

pub(super) async fn import(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<Request>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    super::require_effective_permission(&access, "students.directory.create")?;
    super::require_effective_permission(&access, "authorization.users.create")?;
    super::require_effective_permission(&access, "authorization.users.update")?;
    validate(&request.rows).map_err(ApiError::BadRequest)?;
    let mut results = Vec::new();
    for (i, row) in request.rows.iter().enumerate() {
        let result = create(
            &state,
            &principal.student.tenant_id,
            &principal.student.id,
            row,
        )
        .await;
        match result {Ok(created)=>results.push(json!({"row":i+2,"email":row.email,"status":if created {"created"} else {"already_imported"}})),Err(error)=>{tracing::warn!(error=%error,"student account import row failed");results.push(json!({"row":i+2,"email":row.email,"status":"failed","message":"Not imported. Check for an existing email/roll number or an unknown department. Retry unchanged rows after correcting the issue; existing accounts are never overwritten."}));}}
    }
    Ok(Json(ApiResponse::new(json!({"results":results}))))
}

async fn create(state: &AppState, slug: &str, actor: &str, r: &StudentRow) -> anyhow::Result<bool> {
    use anyhow::{Context, bail};
    let control = state.database().context("PostgreSQL required")?;
    let tenant = state.tenant_database(slug).await?;
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(slug)
        .fetch_one(tenant.pool())
        .await?;
    let control_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(slug)
        .fetch_one(control.pool())
        .await?;
    let role_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM authz.roles WHERE tenant_id=$1 AND role_key='student' AND active",
    )
    .bind(control_id)
    .fetch_one(control.pool())
    .await?;
    let department:String=sqlx::query_scalar("SELECT id::text FROM core.departments WHERE tenant_id=$1 AND (lower(code)=lower($2) OR lower(name)=lower($2) OR id::text=$2)").bind(tenant_id).bind(r.department.trim()).fetch_one(tenant.pool()).await?;
    let email = r.email.trim().to_ascii_lowercase();
    let roll = r.roll_no.trim();
    let key = format!("student-account:{slug}:{}", roll.to_ascii_lowercase());
    let profile = json!({"studentImportKey":key,"onboardingPending":true,"name":r.name.trim(),"rollNumber":roll,"year":format!("Year {}",r.year),"yearOfStudy":r.year,"section":r.section.trim(),"phone":r.mobile_number.trim(),"importedBy":actor});
    let initials: String = r
        .name
        .split_whitespace()
        .take(2)
        .filter_map(|s| s.chars().next())
        .collect::<String>()
        .to_uppercase();
    // Reservation commits first; no membership and inactive means no login.
    sqlx::query("INSERT INTO identity.users(email,password_hash,display_name,initials,account_type,active,profile) VALUES($1,crypt($2,gen_salt('bf',12)),$3,$4,'student',false,$5) ON CONFLICT(email) DO NOTHING").bind(&email).bind(&r.password).bind(r.name.trim()).bind(&initials).bind(&profile).execute(control.pool()).await?;
    let user =
        sqlx::query("SELECT id,password_hash,active,profile FROM identity.users WHERE email=$1")
            .bind(&email)
            .fetch_one(control.pool())
            .await?;
    let user_id: Uuid = user.try_get("id")?;
    let stored: Value = user.try_get("profile")?;
    if stored.get("studentImportKey") != profile.get("studentImportKey") {
        bail!("email already belongs to an existing account");
    }
    let active: bool = user.try_get("active")?;
    if !active && (stored.get("onboardingPending") != Some(&json!(true)) || stored != profile) {
        bail!("existing disabled account or changed pending import");
    }
    let hash: String = user.try_get("password_hash")?;
    let mut tx = tenant.pool().begin().await?;
    // Serialize imports of the same tenant/roll, including case variants.
    // The existing student-number index is case-sensitive.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&key)
        .execute(&mut *tx)
        .await?;
    let existing: Option<Option<Uuid>> = sqlx::query_scalar(
        "SELECT user_account_id FROM core.students WHERE tenant_id=$1 AND lower(student_number)=lower($2)",
    )
    .bind(tenant_id)
    .bind(roll)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some_and(|owner| owner != Some(user_id)) {
        bail!("roll number already belongs to an existing student");
    }
    // Separate tenant databases need the immutable identity UUID for the FK.
    sqlx::query("INSERT INTO identity.users(id,email,password_hash,display_name,initials,account_type,active,profile) VALUES($1,$2,$3,$4,$5,'student',false,$6) ON CONFLICT(id) DO NOTHING").bind(user_id).bind(&email).bind(hash).bind(r.name.trim()).bind(&initials).bind(&profile).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO core.students(tenant_id,student_number,full_name,email,phone,applicant_id,application_id,admission_id,department_id,section_id,user_account_id,profile,status) VALUES($1,$2,$3,$4,$5,$6,$6,$6,$7,$8,$9,$10,'active') ON CONFLICT(tenant_id,student_number) DO NOTHING").bind(tenant_id).bind(roll).bind(r.name.trim()).bind(&email).bind(r.mobile_number.trim()).bind(&key).bind(department).bind(r.section.trim()).bind(user_id).bind(&profile).execute(&mut *tx).await?;
    let owner:Option<Uuid>=sqlx::query_scalar("SELECT user_account_id FROM core.students WHERE tenant_id=$1 AND lower(student_number)=lower($2)").bind(tenant_id).bind(roll).fetch_one(&mut *tx).await?;
    if owner != Some(user_id) {
        bail!("roll number already belongs to an existing student");
    }
    tx.commit().await?;
    if active {
        return Ok(false);
    }
    let mut tx = control.pool().begin().await?;
    sqlx::query("INSERT INTO identity.tenant_memberships(tenant_id,user_id,roles,is_primary,profile) VALUES($1,$2,ARRAY['student'],true,$3) ON CONFLICT(tenant_id,user_id) DO NOTHING").bind(control_id).bind(user_id).bind(&profile).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO authz.user_roles(tenant_id,user_id,role_id,assigned_by) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(control_id).bind(user_id).bind(role_id).bind(actor).execute(&mut *tx).await?;
    sqlx::query("UPDATE identity.users SET active=true,profile=jsonb_set(profile,'{onboardingPending}','false'),updated_at=now() WHERE id=$1 AND profile->>'studentImportKey'=$2").bind(user_id).bind(&key).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> StudentRow {
        StudentRow {
            name: "Example Student".into(),
            roll_no: "413200000001".into(),
            email: "example@college.test".into(),
            mobile_number: "9000000000".into(),
            department: "CSE".into(),
            year: 1,
            section: "A".into(),
            password: "IndividualPassword1!".into(),
        }
    }
    #[test]
    fn validates_year_and_duplicates() {
        assert!(validate(&[row()]).is_ok());
        assert!(validate(&[row(), row()]).is_err());
        let mut r = row();
        r.year = 0;
        assert!(validate(&[r]).is_err());
    }
    #[test]
    fn rejects_empty_and_weak_password() {
        assert!(validate(&[]).is_err());
        let mut r = row();
        r.password = "123".into();
        assert!(validate(&[r]).is_err());
    }

    #[test]
    fn accepts_eight_characters_but_not_seven() {
        let mut r = row();
        r.password = "Abcd1234".into();
        assert!(validate(&[r]).is_ok());
        let mut r = row();
        r.password = "Abcd123".into();
        assert!(validate(&[r]).is_err());
    }
}
