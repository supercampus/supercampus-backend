//! PostgreSQL adapters for the Application Desk.
//!
//! Every query is tenant-scoped: `begin_tenant` resolves the institution slug to
//! its uuid and sets `app.tenant_id` for the transaction, which the row level
//! security policies enforce. A case from one tenant is therefore neither
//! readable nor actionable from another.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use supercampus_database::Database;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::domain::{
    AuditEntry, OnboardingCase, OnboardingEvent, OnboardingServices, ServiceError,
    StudentNumberFormat, StudentNumberInput, WorkflowDefinition, default_workflow, department_code,
    format_student_number, intake::IntakeTriggerMode, sequence_scope,
};

#[derive(Debug, thiserror::Error)]
pub enum DeskError {
    #[error("application desk storage is unavailable")]
    Unavailable,
    #[error("onboarding case {0} was not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("storage error: {0}")]
    Storage(String),
}

impl From<sqlx::Error> for DeskError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

/// Per-tenant desk configuration.
#[derive(Debug, Clone)]
pub struct DeskSettings {
    pub intake_mode: IntakeTriggerMode,
    pub number_format: StudentNumberFormat,
    pub student_role: String,
}

impl Default for DeskSettings {
    fn default() -> Self {
        Self {
            intake_mode: IntakeTriggerMode::OnConfirmed,
            number_format: StudentNumberFormat::default(),
            student_role: "student".into(),
        }
    }
}

#[derive(Clone)]
pub struct PostgresDeskRepository {
    database: Database,
}

impl PostgresDeskRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Resolve the institution slug and scope the transaction to it.
    pub async fn begin_tenant(
        &self,
        tenant_slug: &str,
    ) -> Result<(Uuid, Transaction<'static, Postgres>), DeskError> {
        let tenant_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO platform.tenants (slug, code, name)
               VALUES ($1, upper(replace($1, '-', '_')), $1)
               ON CONFLICT (slug) DO UPDATE SET updated_at = platform.tenants.updated_at
               RETURNING id"#,
        )
        .bind(tenant_slug)
        .fetch_one(self.database.pool())
        .await?;

        let mut transaction = self.database.pool().begin().await?;
        sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
            .bind(tenant_id.to_string())
            .execute(&mut *transaction)
            .await?;
        Ok((tenant_id, transaction))
    }

    /// Load the tenant's active workflow, seeding the default on first use so a
    /// fresh institution is immediately usable.
    pub async fn active_workflow(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        tenant_slug: &str,
    ) -> Result<WorkflowDefinition, DeskError> {
        let existing: Option<Value> = sqlx::query_scalar(
            r#"SELECT definition FROM application_desk.workflows
               WHERE tenant_id = $1 AND active
               ORDER BY version DESC LIMIT 1"#,
        )
        .bind(tenant_id)
        .fetch_optional(&mut **transaction)
        .await?;

        if let Some(definition) = existing {
            return serde_json::from_value(definition).map_err(|error| {
                DeskError::Storage(format!("invalid workflow definition: {error}"))
            });
        }

        let definition = default_workflow(tenant_slug);
        let encoded = serde_json::to_value(&definition)
            .map_err(|error| DeskError::Storage(error.to_string()))?;
        sqlx::query(
            r#"INSERT INTO application_desk.workflows
               (tenant_id, workflow_id, version, name, definition, active)
               VALUES ($1, $2, $3, $4, $5, true)
               ON CONFLICT (tenant_id, workflow_id, version) DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(&definition.id)
        .bind(definition.version)
        .bind(&definition.name)
        .bind(&encoded)
        .execute(&mut **transaction)
        .await?;

        Ok(definition)
    }

    /// A case pins its workflow version, so a mid-flight configuration change
    /// cannot rewrite the rules a case is already running under.
    pub async fn pinned_workflow(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        tenant_slug: &str,
        workflow_id: &str,
        version: i32,
    ) -> Result<WorkflowDefinition, DeskError> {
        let pinned: Option<Value> = sqlx::query_scalar(
            r#"SELECT definition FROM application_desk.workflows
               WHERE tenant_id = $1 AND workflow_id = $2 AND version = $3"#,
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(version)
        .fetch_optional(&mut **transaction)
        .await?;

        match pinned {
            Some(definition) => serde_json::from_value(definition).map_err(|error| {
                DeskError::Storage(format!("invalid workflow definition: {error}"))
            }),
            None => Self::active_workflow(transaction, tenant_id, tenant_slug).await,
        }
    }

    pub async fn settings(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
    ) -> Result<DeskSettings, DeskError> {
        let row: Option<PgRow> = sqlx::query(
            r#"SELECT intake_mode, number_format, student_role
               FROM application_desk.settings WHERE tenant_id = $1"#,
        )
        .bind(tenant_id)
        .fetch_optional(&mut **transaction)
        .await?;

        let Some(row) = row else {
            return Ok(DeskSettings::default());
        };

        let intake_mode: String = row.try_get("intake_mode")?;
        let number_format: Value = row.try_get("number_format")?;
        let student_role: String = row.try_get("student_role")?;

        Ok(DeskSettings {
            intake_mode: IntakeTriggerMode::parse(&intake_mode).unwrap_or_default(),
            number_format: serde_json::from_value(number_format).unwrap_or_default(),
            student_role,
        })
    }

    pub async fn list_cases(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
    ) -> Result<Vec<OnboardingCase>, DeskError> {
        let rows = sqlx::query(
            r#"SELECT document FROM application_desk.cases
               WHERE tenant_id = $1 ORDER BY created_at DESC, id DESC"#,
        )
        .bind(tenant_id)
        .fetch_all(&mut **transaction)
        .await?;

        rows.into_iter()
            .map(|row| {
                let document: Value = row.try_get("document")?;
                serde_json::from_value(document)
                    .map_err(|error| DeskError::Storage(format!("invalid case document: {error}")))
            })
            .collect()
    }

    /// Load one case and hold a row lock for the rest of the transaction, so two
    /// operators acting on the same case serialise rather than interleave.
    pub async fn lock_case(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        case_id: &str,
    ) -> Result<OnboardingCase, DeskError> {
        let row: Option<PgRow> = sqlx::query(
            r#"SELECT document FROM application_desk.cases
               WHERE tenant_id = $1 AND id = $2 FOR UPDATE"#,
        )
        .bind(tenant_id)
        .bind(case_id)
        .fetch_optional(&mut **transaction)
        .await?;

        let row = row.ok_or_else(|| DeskError::NotFound(case_id.to_owned()))?;
        let document: Value = row.try_get("document")?;
        serde_json::from_value(document)
            .map_err(|error| DeskError::Storage(format!("invalid case document: {error}")))
    }

    pub async fn upsert_case(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        onboarding: &OnboardingCase,
    ) -> Result<(), DeskError> {
        let document = serde_json::to_value(onboarding)
            .map_err(|error| DeskError::Storage(error.to_string()))?;
        let student_id = onboarding
            .student_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());
        let user_account_id = onboarding
            .user_account_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());

        let result = sqlx::query(
            r#"INSERT INTO application_desk.cases
               (id, tenant_id, applicant_id, application_id, admission_id, stage, status,
                resume_stage, workflow_id, workflow_version, assigned_to, student_number,
                student_id, user_account_id, document, created_at, updated_at, completed_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
               ON CONFLICT (tenant_id, id) DO UPDATE SET
                   stage = EXCLUDED.stage,
                   status = EXCLUDED.status,
                   resume_stage = EXCLUDED.resume_stage,
                   assigned_to = EXCLUDED.assigned_to,
                   student_number = EXCLUDED.student_number,
                   student_id = EXCLUDED.student_id,
                   user_account_id = EXCLUDED.user_account_id,
                   document = EXCLUDED.document,
                   updated_at = EXCLUDED.updated_at,
                   completed_at = EXCLUDED.completed_at"#,
        )
        .bind(&onboarding.id)
        .bind(tenant_id)
        .bind(&onboarding.applicant_id)
        .bind(&onboarding.application_id)
        .bind(&onboarding.admission_id)
        .bind(onboarding.stage.as_str())
        .bind(onboarding.status.as_str())
        .bind(onboarding.resume_stage.map(|stage| stage.as_str()))
        .bind(&onboarding.workflow_id)
        .bind(onboarding.workflow_version)
        .bind(onboarding.assigned_to.as_deref())
        .bind(onboarding.student_number.as_deref())
        .bind(student_id)
        .bind(user_account_id)
        .bind(&document)
        .bind(onboarding.created_at)
        .bind(onboarding.updated_at)
        .bind(onboarding.completed_at)
        .execute(&mut **transaction)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => Err(
                DeskError::Conflict("A live onboarding case already exists for this applicant, application or admission".into()),
            ),
            Err(error) => Err(error.into()),
        }
    }

    /// Audit rows are written in the same transaction as the state change.
    pub async fn insert_audit(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        entries: &[AuditEntry],
    ) -> Result<(), DeskError> {
        for entry in entries {
            sqlx::query(
                r#"INSERT INTO application_desk.audit_log
                   (tenant_id, case_id, actor, action, from_stage, to_stage,
                    from_status, to_status, reason, created_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
            )
            .bind(tenant_id)
            .bind(&entry.case_id)
            .bind(&entry.actor)
            .bind(&entry.action)
            .bind(entry.from_stage.as_str())
            .bind(entry.to_stage.as_str())
            .bind(entry.from_status.as_str())
            .bind(entry.to_status.as_str())
            .bind(entry.reason.as_deref())
            .bind(entry.timestamp)
            .execute(&mut **transaction)
            .await?;
        }
        Ok(())
    }

    /// Events go to the outbox in the state-change transaction and are published
    /// after commit; downstream modules subscribe rather than being called inline.
    pub async fn enqueue_events(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        events: &[OnboardingEvent],
    ) -> Result<(), DeskError> {
        for event in events {
            let payload = serde_json::to_value(&event.payload)
                .map_err(|error| DeskError::Storage(error.to_string()))?;
            sqlx::query(
                r#"INSERT INTO application_desk.outbox_events
                   (tenant_id, aggregate_id, event_type, payload, created_at)
                   VALUES ($1, $2, $3, $4, $5)"#,
            )
            .bind(tenant_id)
            .bind(&event.case_id)
            .bind(event.name.as_str())
            .bind(&payload)
            .bind(event.timestamp)
            .execute(&mut **transaction)
            .await?;
        }
        Ok(())
    }

    pub async fn recent_audit(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        limit: i64,
    ) -> Result<Vec<Value>, DeskError> {
        let rows = sqlx::query(
            r#"SELECT case_id, actor, action, from_stage, to_stage, from_status,
                      to_status, reason, created_at
               FROM application_desk.audit_log
               WHERE tenant_id = $1 ORDER BY id DESC LIMIT $2"#,
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(&mut **transaction)
        .await?;

        rows.into_iter()
            .map(|row| {
                let timestamp: DateTime<Utc> = row.try_get("created_at")?;
                Ok(json!({
                    "caseId": row.try_get::<String, _>("case_id")?,
                    "actor": row.try_get::<String, _>("actor")?,
                    "action": row.try_get::<String, _>("action")?,
                    "fromStage": row.try_get::<String, _>("from_stage")?,
                    "toStage": row.try_get::<String, _>("to_stage")?,
                    "fromStatus": row.try_get::<String, _>("from_status")?,
                    "toStatus": row.try_get::<String, _>("to_status")?,
                    "reason": row.try_get::<Option<String>, _>("reason")?,
                    "timestamp": timestamp,
                }))
            })
            .collect()
    }

    pub async fn recent_events(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        limit: i64,
    ) -> Result<Vec<Value>, DeskError> {
        let rows = sqlx::query(
            r#"SELECT aggregate_id, event_type, payload, created_at
               FROM application_desk.outbox_events
               WHERE tenant_id = $1 ORDER BY sequence_id DESC LIMIT $2"#,
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(&mut **transaction)
        .await?;

        rows.into_iter()
            .map(|row| {
                let timestamp: DateTime<Utc> = row.try_get("created_at")?;
                Ok(json!({
                    "name": row.try_get::<String, _>("event_type")?,
                    "caseId": row.try_get::<String, _>("aggregate_id")?,
                    "timestamp": timestamp,
                    "payload": row.try_get::<Value, _>("payload")?,
                }))
            })
            .collect()
    }

    /// Allocate the next value in a scope.
    ///
    /// The atomic upsert is the concurrency guarantee: two operators activating
    /// students at the same moment serialise on the row and can never be handed
    /// the same number.
    pub async fn allocate_sequence(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        scope: &str,
    ) -> Result<i64, DeskError> {
        let next: i64 = sqlx::query_scalar(
            r#"INSERT INTO application_desk.number_sequences (tenant_id, scope, next_value)
               VALUES ($1, $2, 1)
               ON CONFLICT (tenant_id, scope) DO UPDATE
                   SET next_value = application_desk.number_sequences.next_value + 1,
                       updated_at = now()
               RETURNING next_value"#,
        )
        .bind(tenant_id)
        .bind(scope)
        .fetch_one(&mut **transaction)
        .await?;
        Ok(next)
    }

    /// Allocate the next human-readable case reference, e.g. `ONB-2026-000145`.
    pub async fn next_case_id(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        year: &str,
    ) -> Result<String, DeskError> {
        let sequence =
            Self::allocate_sequence(transaction, tenant_id, &format!("case:{year}")).await?;
        Ok(format!("ONB-{year}-{sequence:06}"))
    }
}

/// The integration boundary, backed by Postgres.
///
/// Each effect runs inside its own savepoint. When one fails, only that effect
/// is rolled back — the effects already recorded survive, which is what lets a
/// retry reuse the student number instead of burning another.
pub struct PostgresOnboardingServices {
    transaction: Mutex<Transaction<'static, Postgres>>,
    tenant_id: Uuid,
    settings: DeskSettings,
    /// Recorded against role assignments so provisioning is attributable.
    actor: String,
}

impl PostgresOnboardingServices {
    pub fn new(
        transaction: Transaction<'static, Postgres>,
        tenant_id: Uuid,
        settings: DeskSettings,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            transaction: Mutex::new(transaction),
            tenant_id,
            settings,
            actor: actor.into(),
        }
    }

    /// Reclaim the transaction so the caller can persist state and commit.
    pub fn into_transaction(self) -> Transaction<'static, Postgres> {
        self.transaction.into_inner()
    }

    async fn record_effect(
        transaction: &mut Transaction<'static, Postgres>,
        tenant_id: Uuid,
        case_id: &str,
        effect: &str,
        result: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO application_desk.onboarding_effect
               (tenant_id, case_id, effect, result)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT ON CONSTRAINT onboarding_effect_case_effect_key DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(case_id)
        .bind(effect)
        .bind(result)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
    }
}

fn service_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(error.to_string())
}

/// Open a savepoint so one failing effect can be undone without discarding the
/// effects already recorded in this transaction.
async fn savepoint_begin(
    transaction: &mut Transaction<'static, Postgres>,
    name: &str,
) -> Result<(), ServiceError> {
    sqlx::query(&format!("SAVEPOINT {name}"))
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(service_error)
}

async fn savepoint_release(
    transaction: &mut Transaction<'static, Postgres>,
    name: &str,
) -> Result<(), ServiceError> {
    sqlx::query(&format!("RELEASE SAVEPOINT {name}"))
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(service_error)
}

/// Undo just this effect. The rollback also clears the aborted-transaction
/// state a failed statement leaves behind, so the caller can still persist the
/// FAILED case and commit.
async fn savepoint_rollback(transaction: &mut Transaction<'static, Postgres>, name: &str) {
    if let Err(error) = sqlx::query(&format!("ROLLBACK TO SAVEPOINT {name}"))
        .execute(&mut **transaction)
        .await
    {
        tracing::error!(%error, savepoint = name, "failed to roll back onboarding effect");
    }
}

/// Finish one effect: release its savepoint on success, roll it back on failure.
async fn settle<T>(
    transaction: &mut Transaction<'static, Postgres>,
    name: &str,
    outcome: Result<T, ServiceError>,
) -> Result<T, ServiceError> {
    match outcome {
        Ok(value) => {
            savepoint_release(transaction, name).await?;
            Ok(value)
        }
        Err(error) => {
            savepoint_rollback(transaction, name).await;
            Err(error)
        }
    }
}

#[async_trait]
impl OnboardingServices for PostgresOnboardingServices {
    async fn generate_student_number(
        &self,
        onboarding: &OnboardingCase,
        _: &WorkflowDefinition,
    ) -> Result<String, ServiceError> {
        const SAVEPOINT: &str = "effect_generate_number";
        let mut guard = self.transaction.lock().await;
        savepoint_begin(&mut guard, SAVEPOINT).await?;

        let year = onboarding
            .academic_year
            .clone()
            .or_else(|| onboarding.academic.academic_year.clone())
            .unwrap_or_else(|| Utc::now().format("%Y").to_string());
        let department = department_code(onboarding.academic.department_id.as_deref());
        let scope = sequence_scope(&onboarding.tenant_id, &year, &department);

        let outcome = async {
            // The atomic upsert is the concurrency guarantee: two operators
            // activating at the same moment serialise on this row and can never
            // be handed the same sequence value.
            let sequence: i64 = sqlx::query_scalar(
                r#"INSERT INTO application_desk.number_sequences (tenant_id, scope, next_value)
                   VALUES ($1, $2, 1)
                   ON CONFLICT (tenant_id, scope) DO UPDATE
                       SET next_value = application_desk.number_sequences.next_value + 1,
                           updated_at = now()
                   RETURNING next_value"#,
            )
            .bind(self.tenant_id)
            .bind(&scope)
            .fetch_one(&mut **guard)
            .await
            .map_err(service_error)?;

            let student_number = format_student_number(
                &StudentNumberInput {
                    year: year.clone(),
                    department_code: Some(department.clone()),
                    program_code: None,
                    sequence: sequence.max(0) as u64,
                },
                &self.settings.number_format,
            );

            Self::record_effect(
                &mut guard,
                self.tenant_id,
                &onboarding.id,
                "generate_number",
                &student_number,
            )
            .await
            .map_err(service_error)?;

            Ok(student_number)
        }
        .await;

        settle(&mut guard, SAVEPOINT, outcome).await
    }

    async fn create_student(&self, onboarding: &OnboardingCase) -> Result<String, ServiceError> {
        const SAVEPOINT: &str = "effect_create_student";
        let student_number = onboarding
            .student_number
            .clone()
            .ok_or_else(|| ServiceError::new("student number has not been generated"))?;

        let mut guard = self.transaction.lock().await;
        savepoint_begin(&mut guard, SAVEPOINT).await?;

        let outcome = async {
            let student_id: Uuid = sqlx::query_scalar(
                r#"INSERT INTO core.students
                   (tenant_id, student_number, full_name, email, phone, applicant_id,
                    application_id, admission_id, campus_id, department_id, program_id,
                    batch_id, section_id, academic_year, admission_category)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
                   RETURNING id"#,
            )
            .bind(self.tenant_id)
            .bind(&student_number)
            .bind(onboarding.applicant.full_name.clone().unwrap_or_default())
            .bind(onboarding.applicant.email.as_deref())
            .bind(onboarding.applicant.phone.as_deref())
            .bind(&onboarding.applicant_id)
            .bind(&onboarding.application_id)
            .bind(&onboarding.admission_id)
            .bind(onboarding.academic.campus_id.as_deref())
            .bind(onboarding.academic.department_id.as_deref())
            .bind(onboarding.academic.program_id.as_deref())
            .bind(onboarding.academic.batch_id.as_deref())
            .bind(onboarding.academic.section_id.as_deref())
            .bind(onboarding.academic.academic_year.as_deref())
            .bind(onboarding.admission_category.as_deref())
            .fetch_one(&mut **guard)
            .await
            .map_err(service_error)?;

            Self::record_effect(
                &mut guard,
                self.tenant_id,
                &onboarding.id,
                "create_student",
                &student_id.to_string(),
            )
            .await
            .map_err(service_error)?;

            Ok(student_id.to_string())
        }
        .await;

        settle(&mut guard, SAVEPOINT, outcome).await
    }

    async fn create_user_account(
        &self,
        onboarding: &OnboardingCase,
    ) -> Result<String, ServiceError> {
        const SAVEPOINT: &str = "effect_create_user";
        let email = onboarding
            .applicant
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ServiceError::new("applicant email is required to provision an account")
            })?
            .to_ascii_lowercase();
        let display_name = onboarding
            .applicant
            .full_name
            .clone()
            .unwrap_or_else(|| email.clone());
        let student_id = onboarding
            .student_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());

        let mut guard = self.transaction.lock().await;
        savepoint_begin(&mut guard, SAVEPOINT).await?;

        let outcome = async {
            // Created without a usable password: the student sets one through the
            // normal reset flow. The random secret below is never issued.
            let user_id: Uuid = sqlx::query_scalar(
                r#"INSERT INTO identity.users
                   (email, password_hash, display_name, initials, account_type, profile)
                   VALUES ($1, crypt(gen_random_uuid()::text, gen_salt('bf', 12)), $2, $3, 'student', $4)
                   ON CONFLICT (email) DO UPDATE SET
                       display_name = EXCLUDED.display_name,
                       account_type = 'student',
                       active = true,
                       updated_at = now()
                   RETURNING id"#,
            )
            .bind(&email)
            .bind(&display_name)
            .bind(initials(&display_name))
            .bind(json!({
                "studentNumber": onboarding.student_number,
                "onboardingCaseId": onboarding.id,
                "passwordResetRequired": true,
            }))
            .fetch_one(&mut **guard)
            .await
            .map_err(service_error)?;

            if let Some(student_id) = student_id {
                sqlx::query(
                    r#"UPDATE core.students SET user_account_id = $1, updated_at = now()
                       WHERE tenant_id = $2 AND id = $3"#,
                )
                .bind(user_id)
                .bind(self.tenant_id)
                .bind(student_id)
                .execute(&mut **guard)
                .await
                .map_err(service_error)?;
            }

            Self::record_effect(
                &mut guard,
                self.tenant_id,
                &onboarding.id,
                "create_user",
                &user_id.to_string(),
            )
            .await
            .map_err(service_error)?;

            Ok(user_id.to_string())
        }
        .await;

        settle(&mut guard, SAVEPOINT, outcome).await
    }

    async fn provision_access(&self, onboarding: &OnboardingCase) -> Result<(), ServiceError> {
        const SAVEPOINT: &str = "effect_provision_access";
        let user_id = onboarding
            .user_account_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| ServiceError::new("no user account to provision access for"))?;

        let mut guard = self.transaction.lock().await;
        savepoint_begin(&mut guard, SAVEPOINT).await?;

        let outcome = async {
            // Student role plus the modules this tenant has switched on.
            sqlx::query(
                r#"INSERT INTO identity.tenant_memberships
                   (tenant_id, user_id, roles, is_primary, profile)
                   VALUES ($1, $2, ARRAY[$3], true, $4)
                   ON CONFLICT (tenant_id, user_id) DO UPDATE SET
                       roles = EXCLUDED.roles, active = true, updated_at = now()"#,
            )
            .bind(self.tenant_id)
            .bind(user_id)
            .bind(&self.settings.student_role)
            .bind(json!({ "studentNumber": onboarding.student_number }))
            .execute(&mut **guard)
            .await
            .map_err(service_error)?;

            // The role only exists once an institution has defined it; when it
            // does not, membership above is still the effective grant.
            sqlx::query(
                r#"INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
                   SELECT $1, $2, role.id, $4 FROM authz.roles AS role
                   WHERE role.tenant_id = $1 AND role.role_key = $3
                   ON CONFLICT DO NOTHING"#,
            )
            .bind(self.tenant_id)
            .bind(user_id)
            .bind(&self.settings.student_role)
            .bind(&self.actor)
            .execute(&mut **guard)
            .await
            .map_err(service_error)?;

            Self::record_effect(
                &mut guard,
                self.tenant_id,
                &onboarding.id,
                "provision_access",
                "provisioned",
            )
            .await
            .map_err(service_error)?;

            Ok(())
        }
        .await;

        settle(&mut guard, SAVEPOINT, outcome).await
    }

    async fn notify(
        &self,
        onboarding: &OnboardingCase,
        template: &str,
    ) -> Result<(), ServiceError> {
        const SAVEPOINT: &str = "effect_notify";
        let mut guard = self.transaction.lock().await;
        savepoint_begin(&mut guard, SAVEPOINT).await?;

        let outcome = async {
            // There is no notification transport in this backend yet, so the
            // welcome message is published for a subscriber to deliver.
            sqlx::query(
                r#"INSERT INTO application_desk.outbox_events
                   (tenant_id, aggregate_id, event_type, payload)
                   VALUES ($1, $2, 'WelcomeNotificationRequested', $3)"#,
            )
            .bind(self.tenant_id)
            .bind(&onboarding.id)
            .bind(json!({
                "template": template,
                "studentNumber": onboarding.student_number,
                "email": onboarding.applicant.email,
                "guardianEmail": onboarding.applicant.guardian_email,
            }))
            .execute(&mut **guard)
            .await
            .map_err(service_error)?;

            Self::record_effect(&mut guard, self.tenant_id, &onboarding.id, "notify", "sent")
                .await
                .map_err(service_error)?;

            Ok(())
        }
        .await;

        settle(&mut guard, SAVEPOINT, outcome).await
    }
}

fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect();
    if letters.is_empty() {
        "SC".into()
    } else {
        letters.to_uppercase()
    }
}
