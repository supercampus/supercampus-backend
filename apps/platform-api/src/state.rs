use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use supercampus_database::Database;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::models::{
    AuthStudent, ConfigurationDocument, DynamicRecord, ModuleDescriptor, ServiceDescriptor,
    StoredAppState, TenantSummary,
};

#[derive(Clone)]
pub struct AppState {
    modules: Arc<Vec<ModuleDescriptor>>,
    services: Arc<Vec<ServiceDescriptor>>,
    database: Option<Database>,
    records: Arc<RwLock<HashMap<String, DynamicRecord>>>,
    configurations: Arc<RwLock<HashMap<String, ConfigurationDocument>>>,
    sessions: Arc<RwLock<HashMap<String, AuthStudent>>>,
    app_states: Arc<RwLock<HashMap<String, StoredAppState>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            modules: Arc::new(module_catalog()),
            services: Arc::new(service_catalog()),
            database: None,
            records: Arc::new(RwLock::new(HashMap::new())),
            configurations: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            app_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl AppState {
    pub fn with_database(database: Database) -> Self {
        Self {
            database: Some(database),
            ..Self::default()
        }
    }

    pub fn modules(&self) -> Vec<ModuleDescriptor> {
        self.modules.as_ref().clone()
    }

    pub fn module(&self, key: &str) -> Option<ModuleDescriptor> {
        self.modules
            .iter()
            .find(|module| module.key == key)
            .cloned()
    }

    pub fn services(&self) -> Vec<ServiceDescriptor> {
        self.services.as_ref().clone()
    }

    pub fn storage_kind(&self) -> &'static str {
        if self.database.is_some() {
            "postgresql"
        } else {
            "in-memory"
        }
    }

    pub async fn ready(&self) -> anyhow::Result<()> {
        if let Some(database) = &self.database {
            database.ping().await?;
        }
        Ok(())
    }

    pub async fn list_records(
        &self,
        tenant_id: &str,
        module_key: &str,
    ) -> anyhow::Result<Vec<DynamicRecord>> {
        if let Some(database) = &self.database {
            let rows = sqlx::query(
                r#"SELECT r.id, t.slug AS tenant_slug, r.module_key, r.record_type,
                          r.data, r.created_at, r.updated_at
                   FROM platform.dynamic_records r
                   JOIN platform.tenants t ON t.id = r.tenant_id
                   WHERE t.slug = $1 AND r.module_key = $2
                   ORDER BY r.updated_at DESC"#,
            )
            .bind(tenant_id)
            .bind(module_key)
            .fetch_all(database.pool())
            .await
            .context("failed to list module records")?;
            return rows.iter().map(row_to_record).collect();
        }

        Ok(self
            .records
            .read()
            .await
            .values()
            .filter(|record| record.tenant_id == tenant_id && record.module_key == module_key)
            .cloned()
            .collect())
    }

    pub async fn create_record(
        &self,
        tenant_id: String,
        module_key: String,
        record_type: String,
        data: Value,
    ) -> anyhow::Result<DynamicRecord> {
        let now = Utc::now();
        let record = DynamicRecord {
            id: Uuid::new_v4(),
            tenant_id,
            module_key,
            record_type,
            data,
            created_at: now,
            updated_at: now,
        };

        if let Some(database) = &self.database {
            let tenant_uuid = ensure_tenant(database, &record.tenant_id).await?;
            sqlx::query(
                r#"INSERT INTO platform.dynamic_records
                   (id, tenant_id, module_key, record_type, data, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            )
            .bind(record.id)
            .bind(tenant_uuid)
            .bind(&record.module_key)
            .bind(&record.record_type)
            .bind(&record.data)
            .bind(record.created_at)
            .bind(record.updated_at)
            .execute(database.pool())
            .await
            .context("failed to create module record")?;
            return Ok(record);
        }

        self.records.write().await.insert(
            record_key(&record.tenant_id, &record.module_key, record.id),
            record.clone(),
        );
        Ok(record)
    }

    pub async fn record(
        &self,
        tenant_id: &str,
        module_key: &str,
        id: Uuid,
    ) -> anyhow::Result<Option<DynamicRecord>> {
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"SELECT r.id, t.slug AS tenant_slug, r.module_key, r.record_type,
                          r.data, r.created_at, r.updated_at
                   FROM platform.dynamic_records r
                   JOIN platform.tenants t ON t.id = r.tenant_id
                   WHERE t.slug = $1 AND r.module_key = $2 AND r.id = $3"#,
            )
            .bind(tenant_id)
            .bind(module_key)
            .bind(id)
            .fetch_optional(database.pool())
            .await
            .context("failed to read module record")?;
            return row.as_ref().map(row_to_record).transpose();
        }

        Ok(self
            .records
            .read()
            .await
            .get(&record_key(tenant_id, module_key, id))
            .cloned())
    }

    pub async fn update_record(
        &self,
        tenant_id: &str,
        module_key: &str,
        id: Uuid,
        data: Value,
    ) -> anyhow::Result<Option<DynamicRecord>> {
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"UPDATE platform.dynamic_records r
                   SET data = $4, updated_at = now()
                   FROM platform.tenants t
                   WHERE r.tenant_id = t.id AND t.slug = $1
                     AND r.module_key = $2 AND r.id = $3
                   RETURNING r.id, t.slug AS tenant_slug, r.module_key, r.record_type,
                             r.data, r.created_at, r.updated_at"#,
            )
            .bind(tenant_id)
            .bind(module_key)
            .bind(id)
            .bind(data)
            .fetch_optional(database.pool())
            .await
            .context("failed to update module record")?;
            return row.as_ref().map(row_to_record).transpose();
        }

        let mut records = self.records.write().await;
        let Some(record) = records.get_mut(&record_key(tenant_id, module_key, id)) else {
            return Ok(None);
        };
        record.data = data;
        record.updated_at = Utc::now();
        Ok(Some(record.clone()))
    }

    pub async fn delete_record(
        &self,
        tenant_id: &str,
        module_key: &str,
        id: Uuid,
    ) -> anyhow::Result<bool> {
        if let Some(database) = &self.database {
            let result = sqlx::query(
                r#"DELETE FROM platform.dynamic_records r
                   USING platform.tenants t
                   WHERE r.tenant_id = t.id AND t.slug = $1
                     AND r.module_key = $2 AND r.id = $3"#,
            )
            .bind(tenant_id)
            .bind(module_key)
            .bind(id)
            .execute(database.pool())
            .await
            .context("failed to delete module record")?;
            return Ok(result.rows_affected() == 1);
        }

        Ok(self
            .records
            .write()
            .await
            .remove(&record_key(tenant_id, module_key, id))
            .is_some())
    }

    pub async fn configuration(
        &self,
        tenant_id: &str,
        namespace: &str,
    ) -> anyhow::Result<Option<ConfigurationDocument>> {
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"SELECT t.slug AS tenant_slug, d.namespace, d.version, d.value, d.updated_at
                   FROM configuration.runtime_documents d
                   JOIN platform.tenants t ON t.id = d.tenant_id
                   WHERE t.slug = $1 AND d.namespace = $2"#,
            )
            .bind(tenant_id)
            .bind(namespace)
            .fetch_optional(database.pool())
            .await
            .context("failed to read tenant configuration")?;
            return row.as_ref().map(row_to_configuration).transpose();
        }

        Ok(self
            .configurations
            .read()
            .await
            .get(&configuration_key(tenant_id, namespace))
            .cloned())
    }

    pub async fn put_configuration(
        &self,
        tenant_id: String,
        namespace: String,
        value: Value,
    ) -> anyhow::Result<ConfigurationDocument> {
        if let Some(database) = &self.database {
            let tenant_uuid = ensure_tenant(database, &tenant_id).await?;
            let row = sqlx::query(
                r#"INSERT INTO configuration.runtime_documents
                   (tenant_id, namespace, version, value)
                   VALUES ($1, $2, 1, $3)
                   ON CONFLICT (tenant_id, namespace) DO UPDATE
                   SET version = configuration.runtime_documents.version + 1,
                       value = EXCLUDED.value,
                       updated_at = now()
                   RETURNING version, updated_at"#,
            )
            .bind(tenant_uuid)
            .bind(&namespace)
            .bind(&value)
            .fetch_one(database.pool())
            .await
            .context("failed to save tenant configuration")?;
            let version: i64 = row.try_get("version")?;
            return Ok(ConfigurationDocument {
                tenant_id,
                namespace,
                version: u64::try_from(version).context("invalid configuration version")?,
                value,
                updated_at: row.try_get("updated_at")?,
            });
        }

        let mut configurations = self.configurations.write().await;
        let key = configuration_key(&tenant_id, &namespace);
        let version = configurations
            .get(&key)
            .map_or(1, |document| document.version + 1);
        let document = ConfigurationDocument {
            tenant_id,
            namespace,
            version,
            value,
            updated_at: Utc::now(),
        };
        configurations.insert(key, document.clone());
        Ok(document)
    }

    pub async fn create_session(&self, email: String) -> anyhow::Result<(String, AuthStudent)> {
        let token = Uuid::new_v4().to_string();
        let student = local_student(email);

        if let Some(database) = &self.database {
            let tenant_uuid = ensure_tenant(database, &student.tenant_id).await?;
            let student_value =
                serde_json::to_value(&student).context("serialize local student")?;
            sqlx::query(
                r#"INSERT INTO identity.local_sessions
                   (token_hash, tenant_id, user_id, student, expires_at)
                   VALUES (digest($1, 'sha256'), $2, $3, $4, now() + interval '8 hours')"#,
            )
            .bind(&token)
            .bind(tenant_uuid)
            .bind(&student.id)
            .bind(student_value)
            .execute(database.pool())
            .await
            .context("failed to create login session")?;
            return Ok((token, student));
        }

        self.sessions
            .write()
            .await
            .insert(token.clone(), student.clone());
        Ok((token, student))
    }

    pub async fn session(&self, token: &str) -> anyhow::Result<Option<AuthStudent>> {
        if let Some(database) = &self.database {
            let student = sqlx::query_scalar::<_, Value>(
                r#"SELECT student
                   FROM identity.local_sessions
                   WHERE token_hash = digest($1, 'sha256') AND expires_at > now()"#,
            )
            .bind(token)
            .fetch_optional(database.pool())
            .await
            .context("failed to read login session")?;
            return student
                .map(serde_json::from_value)
                .transpose()
                .context("deserialize login session");
        }

        Ok(self.sessions.read().await.get(token).cloned())
    }

    pub async fn remove_session(&self, token: &str) -> anyhow::Result<()> {
        if let Some(database) = &self.database {
            sqlx::query(
                "DELETE FROM identity.local_sessions WHERE token_hash = digest($1, 'sha256')",
            )
            .bind(token)
            .execute(database.pool())
            .await
            .context("failed to revoke login session")?;
            return Ok(());
        }

        self.sessions.write().await.remove(token);
        Ok(())
    }

    pub async fn app_state(&self, student_id: &str) -> anyhow::Result<StoredAppState> {
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"SELECT s.state, s.version, s.updated_at
                   FROM identity.ui_states s
                   JOIN platform.tenants t ON t.id = s.tenant_id
                   WHERE t.slug = 'tenant-local' AND s.user_id = $1"#,
            )
            .bind(student_id)
            .fetch_optional(database.pool())
            .await
            .context("failed to read UI state")?;
            return row
                .as_ref()
                .map(row_to_app_state)
                .transpose()
                .map(|state| state.unwrap_or_else(default_app_state));
        }

        Ok(self
            .app_states
            .read()
            .await
            .get(student_id)
            .cloned()
            .unwrap_or_else(default_app_state))
    }

    pub async fn save_app_state(
        &self,
        student_id: String,
        state: Value,
    ) -> anyhow::Result<StoredAppState> {
        if let Some(database) = &self.database {
            let tenant_uuid = ensure_tenant(database, "tenant-local").await?;
            let row = sqlx::query(
                r#"INSERT INTO identity.ui_states (tenant_id, user_id, state, version)
                   VALUES ($1, $2, $3, 1)
                   ON CONFLICT (tenant_id, user_id) DO UPDATE
                   SET state = EXCLUDED.state,
                       version = identity.ui_states.version + 1,
                       updated_at = now()
                   RETURNING state, version, updated_at"#,
            )
            .bind(tenant_uuid)
            .bind(&student_id)
            .bind(&state)
            .fetch_one(database.pool())
            .await
            .context("failed to save UI state")?;
            return row_to_app_state(&row);
        }

        let mut states = self.app_states.write().await;
        let version = states
            .get(&student_id)
            .map_or(1, |document| document.version + 1);
        let document = StoredAppState {
            state,
            version,
            updated_at: Utc::now(),
        };
        states.insert(student_id, document.clone());
        Ok(document)
    }
}

async fn ensure_tenant(database: &Database, tenant_slug: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar(
        r#"INSERT INTO platform.tenants (slug, name)
           VALUES ($1, $2)
           ON CONFLICT (slug) DO UPDATE SET updated_at = platform.tenants.updated_at
           RETURNING id"#,
    )
    .bind(tenant_slug)
    .bind(tenant_slug)
    .fetch_one(database.pool())
    .await
    .context("failed to resolve tenant")
}

fn row_to_record(row: &PgRow) -> anyhow::Result<DynamicRecord> {
    Ok(DynamicRecord {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_slug")?,
        module_key: row.try_get("module_key")?,
        record_type: row.try_get("record_type")?,
        data: row.try_get("data")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_configuration(row: &PgRow) -> anyhow::Result<ConfigurationDocument> {
    let version: i64 = row.try_get("version")?;
    Ok(ConfigurationDocument {
        tenant_id: row.try_get("tenant_slug")?,
        namespace: row.try_get("namespace")?,
        version: u64::try_from(version).context("invalid configuration version")?,
        value: row.try_get("value")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_app_state(row: &PgRow) -> anyhow::Result<StoredAppState> {
    let version: i64 = row.try_get("version")?;
    Ok(StoredAppState {
        state: row.try_get("state")?,
        version: u64::try_from(version).context("invalid UI state version")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn record_key(tenant_id: &str, module_key: &str, id: Uuid) -> String {
    format!("{tenant_id}:{module_key}:{id}")
}

fn configuration_key(tenant_id: &str, namespace: &str) -> String {
    format!("{tenant_id}:{namespace}")
}

fn local_student(email: String) -> AuthStudent {
    AuthStudent {
        id: "student-local-001".into(),
        tenant_id: "tenant-local".into(),
        email,
        name: "Local Student".into(),
        initials: "LS".into(),
        roll: "SC-LOCAL-001".into(),
        college: "SuperCampus".into(),
        dept: "Computer Science".into(),
        year: "Year 1".into(),
        full_college: "SuperCampus Local Development Institute".into(),
        tenant: TenantSummary {
            id: "tenant-local".into(),
            code: "LOCAL".into(),
            name: "SuperCampus Local".into(),
            city: "Local Development".into(),
        },
    }
}

fn default_app_state() -> StoredAppState {
    StoredAppState {
        state: json!({
            "persona": "hosteller",
            "gp": { "status": "pending", "type": "Weekend Leave", "early": true, "step": 2 },
            "paid": { "tuition": true, "hostel": false, "transport": true, "exam": false },
            "pay": { "comp": null, "step": 0, "plan": null, "mode": null },
            "refunds": {},
            "condonation": "none",
            "examReg": 0,
            "reval": {},
            "asg": { "a3": "none" },
            "changeNotice": true,
            "mess": true,
            "hostelLeave": 0,
            "hostelTickets": [],
            "tripStep": 1,
            "breakdown": true,
            "docReq": [],
            "placeApp": 0,
            "feedback": 0
        }),
        version: 0,
        updated_at: Utc::now(),
    }
}

fn service_catalog() -> Vec<ServiceDescriptor> {
    [
        ("identity", "Identity and authentication", "/api/auth"),
        (
            "authorization",
            "Roles, permissions, and policies",
            "/api/v1/authorization",
        ),
        (
            "configuration",
            "Versioned tenant configuration",
            "/api/v1/configuration",
        ),
        (
            "module-registry",
            "Module registration and lifecycle",
            "/api/v1/modules",
        ),
        (
            "workflow",
            "Workflow definitions and instances",
            "/api/v1/workflows",
        ),
        ("rules", "Business-rule evaluation", "/api/v1/rules"),
        ("audit", "Immutable audit trail", "/api/v1/audit"),
        ("files", "File metadata and storage", "/api/v1/files"),
        (
            "notifications",
            "Notification orchestration",
            "/api/v1/notifications",
        ),
    ]
    .into_iter()
    .map(|(key, name, base_path)| ServiceDescriptor {
        key: key.into(),
        name: name.into(),
        base_path: base_path.into(),
        status: "available".into(),
    })
    .collect()
}

fn module_catalog() -> Vec<ModuleDescriptor> {
    [
        (
            "crm",
            "SuperCampus CRM",
            &["leads", "contacts", "pipelines"][..],
        ),
        (
            "admissions",
            "Admissions",
            &["applications", "offers", "enrollment"],
        ),
        (
            "academics",
            "Academics",
            &["programs", "courses", "curriculum"],
        ),
        (
            "attendance",
            "Attendance",
            &["sessions", "records", "policies"],
        ),
        (
            "documents",
            "Documents",
            &["files", "verification", "retention"],
        ),
        (
            "examinations",
            "Examinations",
            &["assessments", "results", "transcripts"],
        ),
        ("fees", "Fees", &["invoices", "payments", "refunds"]),
        ("gatepass", "Gate Pass", &["passes", "approvals", "scans"]),
        ("hostel", "Hostel", &["rooms", "allocations", "residents"]),
        ("library", "Library", &["catalog", "loans", "fines"]),
        ("placement", "Placement", &["drives", "jobs", "offers"]),
        (
            "transport",
            "Transport",
            &["routes", "vehicles", "tracking"],
        ),
    ]
    .into_iter()
    .map(|(key, name, capabilities)| ModuleDescriptor {
        key: key.into(),
        name: name.into(),
        version: "0.1.0".into(),
        base_path: format!("/api/v1/{key}"),
        status: "active".into(),
        capabilities: capabilities
            .iter()
            .map(|capability| (*capability).into())
            .collect(),
    })
    .collect()
}
