use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use supercampus_authn::{AccessClaims, AuthService, generate_refresh_token, hash_refresh_token};
use supercampus_database::{Database, TenantDatabaseManager};
use supercampus_notifications::{EmailMessage, LogMailer, Mailer};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::models::{
    AssignUserRolesRequest, AuthStudent, ConfigurationDocument, CreateAuthorizationRoleRequest,
    CreateTenantUserRequest, DynamicRecord, ModuleDescriptor, PermissionGrantRequest,
    ServiceDescriptor, SetUserAccessRequest, StoredAppState, TenantSummary,
    UpdateAuthorizationRoleRequest, WorkflowDefinition, WorkflowState, WorkflowStateStatus,
    WorkflowTransition,
};

/// Reset links stay valid for one hour.
const PASSWORD_RESET_TTL_MINUTES: i64 = 60;
/// Requests allowed per account inside [`PASSWORD_RESET_THROTTLE_MINUTES`].
const PASSWORD_RESET_MAX_REQUESTS: i64 = 3;
const PASSWORD_RESET_THROTTLE_MINUTES: i64 = 15;
/// Matches the minimum enforced by the sign-in form.
pub const MINIMUM_PASSWORD_LENGTH: usize = 12;
/// Realtime handshake tokens travel in a URL, so they expire almost immediately.
const REALTIME_TOKEN_TTL_SECONDS: i64 = 60;

#[derive(Debug, Clone)]
struct StoredAuthSession {
    id: Uuid,
    student: AuthStudent,
    roles: Vec<String>,
    refresh_token_hash: [u8; 32],
    previous_refresh_token_hash: Option<[u8; 32]>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct StoredIdentity {
    password: String,
    student: AuthStudent,
    roles: Vec<String>,
}

struct IdentityStudentInput {
    id: String,
    email: String,
    name: String,
    tenant: TenantSummary,
    role: String,
    profile: Value,
}

struct SeedIdentity<'a> {
    email: &'a str,
    password: &'a str,
    display_name: &'a str,
    role: &'a str,
    team: &'a str,
    account_type: &'a str,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedIdentity {
    pub student: AuthStudent,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AuthPrincipal {
    pub session_id: Uuid,
    pub student: AuthStudent,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EffectiveAccess {
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub scopes: HashMap<String, String>,
}

impl EffectiveAccess {
    /// Exact match, the global `*`, or a `namespace.*` grant.
    ///
    /// The namespace form lets a role be granted a whole module — say
    /// `application-desk.*` — without enumerating every permission in it.
    pub fn allows(&self, permission: &str) -> bool {
        self.permissions.iter().any(|value| {
            value == "*"
                || value == permission
                || value.strip_suffix(".*").is_some_and(|namespace| {
                    permission
                        .strip_prefix(namespace)
                        .is_some_and(|rest| rest.starts_with('.'))
                })
        })
    }
}

#[derive(Debug, Clone)]
pub struct CreatedAuthSession {
    pub session_id: Uuid,
    pub student: AuthStudent,
    pub roles: Vec<String>,
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum RefreshSessionResult {
    Rotated(Box<CreatedAuthSession>),
    Invalid,
    ReuseDetected,
}

#[derive(Clone)]
pub struct AppState {
    modules: Arc<Vec<ModuleDescriptor>>,
    services: Arc<Vec<ServiceDescriptor>>,
    database: Option<Database>,
    tenant_databases: Option<TenantDatabaseManager>,
    records: Arc<RwLock<HashMap<String, DynamicRecord>>>,
    configurations: Arc<RwLock<HashMap<String, ConfigurationDocument>>>,
    auth: Arc<AuthService>,
    mailer: Arc<dyn Mailer>,
    sessions: Arc<RwLock<HashMap<Uuid, StoredAuthSession>>>,
    identities: Arc<RwLock<HashMap<String, StoredIdentity>>>,
    app_states: Arc<RwLock<HashMap<String, StoredAppState>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            modules: Arc::new(module_catalog()),
            services: Arc::new(service_catalog()),
            database: None,
            tenant_databases: None,
            records: Arc::new(RwLock::new(HashMap::new())),
            configurations: Arc::new(RwLock::new(HashMap::new())),
            auth: Arc::new(AuthService::default()),
            mailer: Arc::new(LogMailer),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            identities: Arc::new(RwLock::new(HashMap::new())),
            app_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl AppState {
    pub fn with_database(database: Database) -> Self {
        Self {
            database: Some(database.clone()),
            tenant_databases: Some(TenantDatabaseManager::single(database)),
            ..Self::default()
        }
    }

    pub fn with_tenant_databases(databases: TenantDatabaseManager) -> Self {
        Self {
            database: Some(databases.control()),
            tenant_databases: Some(databases),
            ..Self::default()
        }
    }

    pub fn with_auth(mut self, auth: AuthService) -> Self {
        self.auth = Arc::new(auth);
        self
    }

    pub fn with_mailer(mut self, mailer: Arc<dyn Mailer>) -> Self {
        self.mailer = mailer;
        self
    }

    /// Adds an identity only to an in-memory `AppState` (used by isolated tests).
    pub fn with_memory_identity(
        mut self,
        email: &str,
        password: &str,
        tenant_id: &str,
        roles: Vec<String>,
    ) -> Self {
        let email = email.trim().to_ascii_lowercase();
        let primary_role = roles
            .first()
            .cloned()
            .unwrap_or_else(|| "unassigned".into());
        let student = identity_student(IdentityStudentInput {
            id: Uuid::new_v4().to_string(),
            email: email.clone(),
            name: display_name_from_email(&email),
            tenant: TenantSummary {
                id: tenant_id.to_owned(),
                code: tenant_id.to_uppercase(),
                name: tenant_id.to_owned(),
                city: String::new(),
            },
            role: primary_role,
            profile: Value::Object(Default::default()),
        });
        let mut identities = HashMap::new();
        identities.insert(
            identity_key(&email, tenant_id),
            StoredIdentity {
                password: password.to_owned(),
                student,
                roles,
            },
        );
        let identity_store = Arc::get_mut(&mut self.identities)
            .expect("with_memory_identity must be called before AppState is cloned");
        identity_store.get_mut().extend(identities);
        self
    }

    pub async fn tenants(&self) -> anyhow::Result<Vec<TenantSummary>> {
        if let Some(database) = &self.database {
            let rows = sqlx::query(
                "SELECT slug, code, name, city FROM platform.tenants WHERE status = 'active' ORDER BY name",
            )
            .fetch_all(database.pool())
            .await
            .context("failed to list login tenants")?;
            return rows.iter().map(row_to_tenant).collect();
        }
        let identities = self.identities.read().await;
        let mut tenants = identities
            .values()
            .map(|identity| identity.student.tenant.clone())
            .collect::<Vec<_>>();
        tenants.sort_by(|left, right| left.name.cmp(&right.name));
        tenants.dedup_by(|left, right| left.id == right.id);
        Ok(tenants)
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

    pub fn database(&self) -> Option<Database> {
        self.database.clone()
    }

    pub fn tenant_databases(&self) -> Option<TenantDatabaseManager> {
        self.tenant_databases.clone()
    }

    pub async fn tenant_database(&self, tenant_slug: &str) -> anyhow::Result<Database> {
        match &self.tenant_databases {
            Some(databases) => databases.tenant(tenant_slug).await,
            None => self
                .database
                .clone()
                .context("PostgreSQL is required for tenant storage"),
        }
    }

    pub fn storage_kind(&self) -> &'static str {
        if self.database.is_some() {
            "postgresql"
        } else {
            "in-memory"
        }
    }

    pub async fn ready(&self) -> anyhow::Result<()> {
        if let Some(databases) = &self.tenant_databases {
            databases.ping_registered().await?;
        } else if let Some(database) = &self.database {
            database.ping().await?;
        }
        Ok(())
    }

    pub async fn list_records(
        &self,
        tenant_id: &str,
        module_key: &str,
    ) -> anyhow::Result<Vec<DynamicRecord>> {
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
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

        if self.database.is_some() {
            let database = self.tenant_database(&record.tenant_id).await?;
            let tenant_uuid = ensure_tenant(&database, &record.tenant_id).await?;
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
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
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
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
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
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
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
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
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
        if self.database.is_some() {
            let database = self.tenant_database(&tenant_id).await?;
            let tenant_uuid = ensure_tenant(&database, &tenant_id).await?;
            let row = sqlx::query(
                r#"INSERT INTO configuration.runtime_documents
                   (tenant_id, namespace, version, value)
                   VALUES ($1, $2, 1, $3)
                   ON CONFLICT (tenant_id, namespace) DO UPDATE
                   SET version = EXCLUDED.version + 1,
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

    pub async fn workflow_definition(
        &self,
        tenant_id: &str,
        module: &str,
        feature: &str,
    ) -> anyhow::Result<Option<WorkflowDefinition>> {
        let namespace = workflow_namespace(module, feature);
        if let Some(document) = self.configuration(tenant_id, &namespace).await? {
            let mut definition: WorkflowDefinition = serde_json::from_value(document.value)
                .with_context(|| {
                    format!("invalid workflow configuration for {tenant_id}:{namespace}")
                })?;
            definition.tenant_id = tenant_id.to_owned();
            return Ok(Some(definition));
        }
        Ok(seed_workflow_definition(tenant_id, module, feature))
    }

    pub async fn authenticate_credentials(
        &self,
        email: &str,
        password: &str,
    ) -> anyhow::Result<Option<AuthenticatedIdentity>> {
        let email = email.trim().to_ascii_lowercase();
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"SELECT u.id::text AS user_id, u.email, u.display_name, u.initials,
                          m.roles, m.profile, t.slug, t.code, t.name, t.city
                   FROM identity.users u
                   JOIN identity.tenant_memberships m ON m.user_id = u.id
                   JOIN platform.tenants t ON t.id = m.tenant_id
                   WHERE u.email = $1
                     AND u.password_hash = crypt($2, u.password_hash)
                     AND u.active AND m.active AND t.status = 'active'
                   ORDER BY m.is_primary DESC, t.name
                   LIMIT 1"#,
            )
            .bind(&email)
            .bind(password)
            .fetch_optional(database.pool())
            .await
            .context("failed to authenticate identity")?;
            let Some(row) = row else {
                return Ok(None);
            };
            let roles: Vec<String> = row.try_get("roles")?;
            let primary_role = roles
                .first()
                .cloned()
                .unwrap_or_else(|| "unassigned".into());
            let profile: Value = row.try_get("profile")?;
            let mut student = identity_student(IdentityStudentInput {
                id: row.try_get("user_id")?,
                email: row.try_get("email")?,
                name: row.try_get("display_name")?,
                tenant: TenantSummary {
                    id: row.try_get("slug")?,
                    code: row.try_get("code")?,
                    name: row.try_get("name")?,
                    city: row
                        .try_get::<Option<String>, _>("city")?
                        .unwrap_or_default(),
                },
                role: primary_role,
                profile,
            });
            sqlx::query("UPDATE identity.users SET last_login_at = now() WHERE id = $1::uuid")
                .bind(&student.id)
                .execute(database.pool())
                .await
                .context("failed to update identity login time")?;
            let access = self
                .effective_access(&student.tenant_id, &student.id)
                .await?;
            let roles = if access.roles.is_empty() {
                roles
            } else {
                access.roles
            };
            student.role = roles.first().cloned().unwrap_or_default();
            student.access = access.permissions;
            return Ok(Some(AuthenticatedIdentity { student, roles }));
        }

        let identities = self.identities.read().await;
        let identity = identities
            .values()
            .find(|identity| identity.student.email == email);
        Ok(identity
            .filter(|identity| identity.password == password)
            .map(|identity| AuthenticatedIdentity {
                student: identity.student.clone(),
                roles: identity.roles.clone(),
            }))
    }

    pub async fn effective_access(
        &self,
        tenant_slug: &str,
        user_id: &str,
    ) -> anyhow::Result<EffectiveAccess> {
        self.effective_access_for_surface(tenant_slug, user_id, "app")
            .await
    }

    pub async fn effective_access_for_surface(
        &self,
        tenant_slug: &str,
        user_id: &str,
        surface: &str,
    ) -> anyhow::Result<EffectiveAccess> {
        if let Some(database) = &self.database {
            let rows = sqlx::query(
                r#"SELECT role.role_key, role_grant.permission_key, role_grant.scope,
                          'allow'::text AS mode
                   FROM platform.tenants tenant
                   JOIN identity.tenant_memberships membership
                       ON membership.tenant_id = tenant.id
                       AND membership.user_id = $2::uuid
                       AND membership.active
                   JOIN authz.roles role ON role.tenant_id = tenant.id
                       AND role.role_key = ANY(membership.roles)
                       AND role.active
                   LEFT JOIN authz.role_permissions role_grant ON role_grant.tenant_id = tenant.id
                       AND role_grant.role_id = role.id
                   LEFT JOIN authz.permission_definitions permission
                       ON permission.tenant_id = tenant.id
                       AND permission.permission_key = role_grant.permission_key
                       AND permission.active
                   WHERE tenant.slug = $1
                   UNION ALL
                   SELECT NULL::text AS role_key, assignment.permission AS permission_key,
                          assignment.scope, assignment.mode
                   FROM platform.tenants tenant
                   JOIN authz.assignments assignment ON assignment.tenant_id = tenant.id
                       AND assignment.principal_id = $2::uuid
                       AND assignment.surface = $3
                       AND assignment.active
                   JOIN authz.permission_definitions permission
                       ON permission.tenant_id = tenant.id
                       AND permission.permission_key = assignment.permission
                       AND permission.active
                   WHERE tenant.slug = $1
                   ORDER BY 1 NULLS LAST, 2"#,
            )
            .bind(tenant_slug)
            .bind(user_id)
            .bind(surface)
            .fetch_all(database.pool())
            .await
            .context("failed to load effective tenant access")?;

            let mut roles = Vec::new();
            let mut permissions = Vec::new();
            let mut scopes: HashMap<String, String> = HashMap::new();
            let mut denied_permissions = Vec::new();
            for row in rows {
                let role_key: Option<String> = row.try_get("role_key")?;
                if let Some(role_key) = role_key
                    && !roles.contains(&role_key)
                {
                    roles.push(role_key);
                }
                let permission_key: Option<String> = row.try_get("permission_key")?;
                let scope: Option<String> = row.try_get("scope")?;
                let mode: String = row.try_get("mode")?;
                if let Some(permission_key) = permission_key {
                    if mode == "deny" {
                        if !denied_permissions.contains(&permission_key) {
                            denied_permissions.push(permission_key.clone());
                        }
                        continue;
                    }
                    if !permissions.contains(&permission_key) {
                        permissions.push(permission_key.clone());
                    }
                    let next_scope = scope.unwrap_or_else(|| "all".into());
                    let should_replace = scopes.get(&permission_key).is_none_or(|current| {
                        access_scope_rank(&next_scope) > access_scope_rank(current)
                    });
                    if should_replace {
                        scopes.insert(permission_key, next_scope);
                    }
                }
            }
            if !denied_permissions.is_empty() {
                permissions.retain(|permission| !denied_permissions.contains(permission));
                for permission in denied_permissions {
                    scopes.remove(&permission);
                }
            }

            if roles.is_empty() {
                roles = sqlx::query_scalar(
                    r#"SELECT membership.roles
                       FROM platform.tenants tenant
                       JOIN identity.tenant_memberships membership ON membership.tenant_id = tenant.id
                       WHERE tenant.slug = $1 AND membership.user_id = $2::uuid AND membership.active"#,
                )
                .bind(tenant_slug)
                .bind(user_id)
                .fetch_optional(database.pool())
                .await
                .context("failed to load legacy tenant roles")?
                .unwrap_or_default();
            }

            return Ok(EffectiveAccess {
                roles,
                permissions,
                scopes,
            });
        }

        let identities = self.identities.read().await;
        let identity = identities.values().find(|identity| {
            identity.student.tenant_id == tenant_slug && identity.student.id == user_id
        });
        Ok(identity.map_or(
            EffectiveAccess {
                roles: Vec::new(),
                permissions: Vec::new(),
                scopes: HashMap::new(),
            },
            |identity| EffectiveAccess {
                roles: identity.roles.clone(),
                permissions: vec!["*".into()],
                scopes: HashMap::from([("*".into(), "all".into())]),
            },
        ))
    }

    pub async fn tenant_user_access(
        &self,
        tenant_slug: &str,
        user_id: Uuid,
        surface: &str,
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let rows = sqlx::query(
            r#"SELECT assignment.permission AS key, assignment.scope,
                      assignment.mode, assignment.constraints, permission.module_key AS "moduleKey",
                      permission.feature_key AS "featureKey",
                      permission.crud_actions AS "crudActions"
               FROM authz.assignments assignment
               JOIN platform.tenants tenant ON tenant.id = assignment.tenant_id
               JOIN authz.permission_definitions permission
                 ON permission.tenant_id = assignment.tenant_id
                AND permission.permission_key = assignment.permission
                AND permission.active
               WHERE tenant.slug = $1 AND assignment.principal_id = $2
                 AND assignment.surface = $3 AND assignment.active
               ORDER BY permission.module_key, permission.feature_key, assignment.permission"#,
        )
        .bind(tenant_slug)
        .bind(user_id)
        .bind(surface)
        .fetch_all(database.pool())
        .await
        .context("failed to list direct user access")?;
        let grants = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "key": row.try_get::<String, _>("key")?,
                    "scope": row.try_get::<String, _>("scope")?,
                    "mode": row.try_get::<String, _>("mode")?,
                    "constraints": row.try_get::<Value, _>("constraints")?,
                    "moduleKey": row.try_get::<String, _>("moduleKey")?,
                    "featureKey": row.try_get::<String, _>("featureKey")?,
                    "crudActions": row.try_get::<Vec<String>, _>("crudActions")?,
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!({ "surface": surface, "userId": user_id, "grants": grants }))
    }

    pub async fn set_tenant_user_access(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        user_id: Uuid,
        request: &SetUserAccessRequest,
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let actor_id = Uuid::parse_str(actor_id).context("invalid actor id")?;
        let mut transaction = database.pool().begin().await?;
        let member_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM identity.tenant_memberships WHERE tenant_id = $1 AND user_id = $2 AND active)",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !member_exists {
            bail!("user does not belong to this tenant");
        }
        sqlx::query(
            "DELETE FROM authz.assignments WHERE tenant_id = $1 AND principal_id = $2 AND surface = $3",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&request.surface)
        .execute(&mut *transaction)
        .await?;
        for grant in &request.grants {
            sqlx::query(
                r#"INSERT INTO authz.assignments
                   (tenant_id, principal_id, permission, constraints, surface, scope, mode, granted_by, active)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true)"#,
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(grant.key.trim())
            .bind(&grant.constraints)
            .bind(&request.surface)
            .bind(&grant.scope)
            .bind(&grant.mode)
            .bind(actor_id.to_string())
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(json!({
            "userId": user_id,
            "surface": request.surface,
            "grantCount": request.grants.len(),
        }))
    }

    pub async fn authorization_permissions(&self, tenant_slug: &str) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let rows = sqlx::query(
            r#"SELECT permission.permission_key, permission.module_key, permission.feature_key,
                      permission.action, permission.crud_actions,
                      permission.display_name, permission.description,
                      permission.active
               FROM authz.permission_definitions permission
               JOIN platform.tenants tenant ON tenant.id = permission.tenant_id
               WHERE tenant.slug = $1
               ORDER BY permission.module_key, permission.feature_key, permission.action"#,
        )
        .bind(tenant_slug)
        .fetch_all(database.pool())
        .await
        .context("failed to list tenant permissions")?;
        let permissions = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "key": row.try_get::<String, _>("permission_key")?,
                    "moduleKey": row.try_get::<String, _>("module_key")?,
                    "featureKey": row.try_get::<String, _>("feature_key")?,
                    "action": row.try_get::<String, _>("action")?,
                    "crudActions": row.try_get::<Vec<String>, _>("crud_actions")?,
                    "name": row.try_get::<String, _>("display_name")?,
                    "description": row.try_get::<String, _>("description")?,
                    "active": row.try_get::<bool, _>("active")?,
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!(permissions))
    }

    pub async fn authorization_permission_keys_exist(
        &self,
        tenant_slug: &str,
        permission_keys: &[String],
    ) -> anyhow::Result<bool> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let count: i64 = sqlx::query_scalar(
            r#"SELECT count(*)
               FROM authz.permission_definitions
               WHERE tenant_id = $1 AND active AND permission_key = ANY($2)"#,
        )
        .bind(tenant_id)
        .bind(permission_keys)
        .fetch_one(database.pool())
        .await
        .context("failed to validate tenant permission keys")?;
        Ok(count as usize == permission_keys.len())
    }

    pub async fn authorization_roles(&self, tenant_slug: &str) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let rows = sqlx::query(
            r#"SELECT role.id, role.role_key, role.name, role.team, role.scope_description,
                      role.protected, role.active,
                      COALESCE((
                          SELECT jsonb_agg(jsonb_build_object(
                              'key', role_grant.permission_key,
                              'scope', role_grant.scope,
                              'constraints', role_grant.constraints
                          ) ORDER BY role_grant.permission_key)
                          FROM authz.role_permissions role_grant
                          WHERE role_grant.tenant_id = role.tenant_id AND role_grant.role_id = role.id
                      ), '[]'::jsonb) AS permissions
               FROM authz.roles role
               JOIN platform.tenants tenant ON tenant.id = role.tenant_id
               WHERE tenant.slug = $1
               ORDER BY role.protected DESC, role.name"#,
        )
        .bind(tenant_slug)
        .fetch_all(database.pool())
        .await
        .context("failed to list tenant roles")?;
        let roles = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "id": row.try_get::<Uuid, _>("id")?,
                    "key": row.try_get::<String, _>("role_key")?,
                    "name": row.try_get::<String, _>("name")?,
                    "team": row.try_get::<String, _>("team")?,
                    "scope": row.try_get::<String, _>("scope_description")?,
                    "protected": row.try_get::<bool, _>("protected")?,
                    "active": row.try_get::<bool, _>("active")?,
                    "permissions": row.try_get::<Value, _>("permissions")?,
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!(roles))
    }

    pub async fn create_authorization_role(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        request: &CreateAuthorizationRoleRequest,
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO authz.roles
               (tenant_id, role_key, name, team, scope_description, created_by, updated_by)
               VALUES ($1, $2, $3, $4, $5, $6, $6)
               RETURNING id, role_key, name, team, scope_description, protected, active"#,
        )
        .bind(tenant_id)
        .bind(request.key.trim())
        .bind(request.name.trim())
        .bind(if request.team.trim().is_empty() {
            "Custom"
        } else {
            request.team.trim()
        })
        .bind(request.scope.trim())
        .bind(actor_id)
        .fetch_one(database.pool())
        .await
        .context("failed to create tenant role")?;
        authorization_role_row(&row, json!([]))
    }

    pub async fn update_authorization_role(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        role_id: Uuid,
        request: &UpdateAuthorizationRoleRequest,
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let row = sqlx::query(
            r#"UPDATE authz.roles role
               SET name = COALESCE($3, role.name), team = COALESCE($4, role.team),
                   scope_description = COALESCE($5, role.scope_description),
                   active = COALESCE($6, role.active), updated_by = $7, updated_at = now()
               FROM platform.tenants tenant
               WHERE role.tenant_id = tenant.id AND tenant.slug = $1 AND role.id = $2
                 AND NOT role.protected
               RETURNING role.id, role.role_key, role.name, role.team,
                         role.scope_description, role.protected, role.active"#,
        )
        .bind(tenant_slug)
        .bind(role_id)
        .bind(request.name.as_deref())
        .bind(request.team.as_deref())
        .bind(request.scope.as_deref())
        .bind(request.active)
        .bind(actor_id)
        .fetch_optional(database.pool())
        .await
        .context("failed to update tenant role")?
        .context("role was not found or is protected")?;
        authorization_role_row(&row, json!([]))
    }

    pub async fn set_authorization_role_permissions(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        role_id: Uuid,
        permissions: &[PermissionGrantRequest],
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let protected: Option<bool> = sqlx::query_scalar(
            "SELECT protected FROM authz.roles WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(role_id)
        .fetch_optional(database.pool())
        .await
        .context("failed to resolve tenant role")?;
        if protected.context("role not found")? {
            bail!("protected tenant roles cannot be modified");
        }

        let mut transaction = database.pool().begin().await?;
        sqlx::query("DELETE FROM authz.role_permissions WHERE tenant_id = $1 AND role_id = $2")
            .bind(tenant_id)
            .bind(role_id)
            .execute(&mut *transaction)
            .await?;
        for grant in permissions {
            let inserted = sqlx::query(
                r#"INSERT INTO authz.role_permissions
                   (tenant_id, role_id, permission_key, scope, constraints, granted_by)
                   SELECT $1, $2, permission.permission_key, $4, $5, $6
                   FROM authz.permission_definitions permission
                   WHERE permission.tenant_id = $1 AND permission.permission_key = $3 AND permission.active"#,
            )
            .bind(tenant_id)
            .bind(role_id)
            .bind(grant.key.trim())
            .bind(grant.scope.trim())
            .bind(&grant.constraints)
            .bind(actor_id)
            .execute(&mut *transaction)
            .await?;
            if inserted.rows_affected() != 1 {
                bail!("permission key is not active for this tenant");
            }
        }
        transaction.commit().await?;
        self.authorization_roles(tenant_slug)
            .await
            .and_then(|roles| {
                roles
                    .as_array()
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|role| role.get("id") == Some(&json!(role_id)))
                            .cloned()
                    })
                    .context("updated role not found")
            })
    }

    pub async fn delete_authorization_role(
        &self,
        tenant_slug: &str,
        _actor_id: &str,
        role_id: Uuid,
    ) -> anyhow::Result<()> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let mut transaction = database.pool().begin().await?;
        let row = sqlx::query(
            "SELECT role_key, protected FROM authz.roles WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(role_id)
        .fetch_optional(&mut *transaction)
        .await?
        .context("role not found")?;
        let role_key: String = row.try_get("role_key")?;
        let protected: bool = row.try_get("protected")?;
        if protected {
            bail!("protected tenant roles cannot be deleted");
        }
        sqlx::query(
            "UPDATE identity.tenant_memberships SET roles = array_remove(roles, $2), updated_at = now() WHERE tenant_id = $1 AND $2 = ANY(roles)",
        )
        .bind(tenant_id)
        .bind(&role_key)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM authz.roles WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(role_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn tenant_users(&self, tenant_slug: &str) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let rows = sqlx::query(
            r#"SELECT user_account.id, user_account.email, user_account.display_name,
                      user_account.initials, user_account.account_type, user_account.active,
                      COALESCE((
                          SELECT jsonb_agg(jsonb_build_object(
                              'id', role.id, 'key', role.role_key, 'name', role.name,
                              'team', role.team
                          ) ORDER BY role.name)
                          FROM authz.user_roles user_role
                          JOIN authz.roles role ON role.id = user_role.role_id
                              AND role.tenant_id = user_role.tenant_id
                          WHERE user_role.tenant_id = membership.tenant_id
                            AND user_role.user_id = membership.user_id
                      ), '[]'::jsonb) AS roles
               FROM identity.tenant_memberships membership
               JOIN platform.tenants tenant ON tenant.id = membership.tenant_id
               JOIN identity.users user_account ON user_account.id = membership.user_id
               WHERE tenant.slug = $1 AND membership.active
               ORDER BY user_account.display_name"#,
        )
        .bind(tenant_slug)
        .fetch_all(database.pool())
        .await
        .context("failed to list tenant users")?;
        let users = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "id": row.try_get::<Uuid, _>("id")?,
                    "email": row.try_get::<String, _>("email")?,
                    "name": row.try_get::<String, _>("display_name")?,
                    "initials": row.try_get::<String, _>("initials")?,
                    "accountType": row.try_get::<String, _>("account_type")?,
                    "active": row.try_get::<bool, _>("active")?,
                    "roles": row.try_get::<Value, _>("roles")?,
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!(users))
    }

    pub async fn create_tenant_user(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        request: &CreateTenantUserRequest,
    ) -> anyhow::Result<Option<Value>> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let email = request.email.trim().to_ascii_lowercase();
        let password = request
            .credential_password()
            .context("an explicit password is required")?;
        let mut transaction = database.pool().begin().await?;

        let role_keys: Vec<String> = sqlx::query_scalar(
            "SELECT role_key FROM authz.roles WHERE tenant_id = $1 AND id = ANY($2) AND active ORDER BY name",
        )
        .bind(tenant_id)
        .bind(&request.role_ids)
        .fetch_all(&mut *transaction)
        .await?;
        if role_keys.len() != request.role_ids.len() {
            bail!("one or more roles do not belong to this tenant");
        }

        let existing_user: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM identity.users WHERE email = $1")
                .bind(&email)
                .fetch_optional(&mut *transaction)
                .await?;
        let created = existing_user.is_none();
        let user_id = if let Some(user_id) = existing_user {
            sqlx::query(
                r#"UPDATE identity.users
                   SET password_hash = crypt($2, gen_salt('bf', 12)),
                       display_name = $3,
                       initials = $4,
                       active = true,
                       updated_at = now()
                   WHERE id = $1"#,
            )
            .bind(user_id)
            .bind(password)
            .bind(request.name.trim())
            .bind(initials(request.name.trim()))
            .execute(&mut *transaction)
            .await?;
            user_id
        } else {
            sqlx::query_scalar(
                r#"INSERT INTO identity.users
                   (email, password_hash, display_name, initials, account_type)
                   VALUES ($1, crypt($2, gen_salt('bf', 12)), $3, $4, 'staff')
                   RETURNING id"#,
            )
            .bind(&email)
            .bind(password)
            .bind(request.name.trim())
            .bind(initials(request.name.trim()))
            .fetch_one(&mut *transaction)
            .await?
        };

        sqlx::query(
            r#"INSERT INTO identity.tenant_memberships
               (tenant_id, user_id, roles, is_primary, profile)
               VALUES ($1, $2, $3, true, '{}'::jsonb)
               ON CONFLICT (tenant_id, user_id) DO UPDATE SET
                   roles = EXCLUDED.roles,
                   active = true,
                   updated_at = now()"#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&role_keys)
        .execute(&mut *transaction)
        .await?;
        replace_user_roles(
            &mut transaction,
            tenant_id,
            user_id,
            &request.role_ids,
            actor_id,
        )
        .await?;
        transaction.commit().await?;

        Ok(Some(json!({
            "id": user_id,
            "email": email,
            "name": request.name.trim(),
            "roleIds": request.role_ids,
            "created": created,
        })))
    }

    pub async fn assign_tenant_user_roles(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        user_id: Uuid,
        request: &AssignUserRolesRequest,
    ) -> anyhow::Result<Value> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let tenant_id = ensure_tenant(database, tenant_slug).await?;
        let mut transaction = database.pool().begin().await?;
        let role_keys: Vec<String> = sqlx::query_scalar(
            "SELECT role_key FROM authz.roles WHERE tenant_id = $1 AND id = ANY($2) AND active ORDER BY name",
        )
        .bind(tenant_id)
        .bind(&request.role_ids)
        .fetch_all(&mut *transaction)
        .await?;
        if role_keys.len() != request.role_ids.len() {
            bail!("one or more roles do not belong to this tenant");
        }
        sqlx::query(
            "UPDATE identity.tenant_memberships SET roles = $3, updated_at = now() WHERE tenant_id = $1 AND user_id = $2 AND active",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&role_keys)
        .execute(&mut *transaction)
        .await?;
        replace_user_roles(
            &mut transaction,
            tenant_id,
            user_id,
            &request.role_ids,
            actor_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(json!({ "userId": user_id, "roleIds": request.role_ids }))
    }

    pub async fn seed_test_identities_from_environment(&self) -> anyhow::Result<usize> {
        if !environment_flag("SEED_TEST_USERS") {
            return Ok(0);
        }
        let database = self
            .database
            .as_ref()
            .context("test identity seeding requires PostgreSQL")?;
        let tenant_slug =
            std::env::var("TEST_TENANT_SLUG").unwrap_or_else(|_| "tenant-local".into());
        let tenant_name =
            std::env::var("TEST_TENANT_NAME").unwrap_or_else(|_| "SuperCampus Institution".into());
        let tenant_code =
            std::env::var("TEST_TENANT_CODE").unwrap_or_else(|_| tenant_slug.to_uppercase());
        let tenant_city = std::env::var("TEST_TENANT_CITY").unwrap_or_default();
        let tenant_uuid: Uuid = sqlx::query_scalar(
            r#"INSERT INTO platform.tenants (slug, code, name, city)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (slug) DO UPDATE SET code = EXCLUDED.code, name = EXCLUDED.name,
                   city = EXCLUDED.city, status = 'active', updated_at = now()
               RETURNING id"#,
        )
        .bind(&tenant_slug)
        .bind(&tenant_code)
        .bind(&tenant_name)
        .bind(&tenant_city)
        .fetch_one(database.pool())
        .await
        .context("failed to seed test tenant")?;

        let specifications = [
            (
                "TEST_ADMIN",
                "tenant_admin",
                "Admissions Test Admin",
                "Admissions",
                "staff",
            ),
            (
                "TEST_COUNSELOR",
                "tenant_member",
                "Admissions Test Counselor",
                "Admissions",
                "staff",
            ),
            (
                "TEST_STUDENT",
                "tenant_member",
                "Test Student",
                "Students",
                "student",
            ),
        ];
        let mut seeded = 0;
        for (prefix, default_role, default_name, team, account_type) in specifications {
            let email_key = format!("{prefix}_EMAIL");
            let password_key = format!("{prefix}_PASSWORD");
            let email = match std::env::var(&email_key) {
                Ok(val) if !val.trim().is_empty() => val,
                _ if prefix == "TEST_ADMIN" => "tenant.admin@supercampus.local".into(),
                _ => continue,
            };
            let password = match std::env::var(&password_key) {
                Ok(val) if val.len() >= 12 => val,
                _ if prefix == "TEST_ADMIN" => "SuperCampus@Test2026".into(),
                _ => continue,
            };
            let name =
                std::env::var(format!("{prefix}_NAME")).unwrap_or_else(|_| default_name.into());
            let role =
                std::env::var(format!("{prefix}_ROLE")).unwrap_or_else(|_| default_role.into());
            let identity = SeedIdentity {
                email: &email,
                password: &password,
                display_name: &name,
                role: &role,
                team,
                account_type,
            };
            seed_identity(database, tenant_uuid, &identity).await?;
            seeded += 1;
        }
        if seeded == 0 {
            bail!("SEED_TEST_USERS is enabled but no TEST_*_EMAIL/PASSWORD pair is configured");
        }
        Ok(seeded)
    }

    pub async fn create_session(
        &self,
        identity: AuthenticatedIdentity,
    ) -> anyhow::Result<CreatedAuthSession> {
        let session_id = Uuid::new_v4();
        let student = identity.student;
        let roles = identity.roles;
        let refresh_token = generate_refresh_token();
        let refresh_token_hash = hash_refresh_token(&refresh_token);
        let refresh_expires_at = self.auth.refresh_expires_at();
        let access = self
            .auth
            .issue_access_token(&student.id, &student.tenant_id, session_id, roles.clone())
            .context("failed to issue access token")?;

        if let Some(database) = &self.database {
            let tenant_uuid = ensure_tenant(database, &student.tenant_id).await?;
            let profile = serde_json::to_value(&student).context("serialize local student")?;
            sqlx::query(
                r#"INSERT INTO identity.auth_sessions
                   (id, tenant_id, user_id, roles, profile, refresh_token_hash, expires_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            )
            .bind(session_id)
            .bind(tenant_uuid)
            .bind(&student.id)
            .bind(&roles)
            .bind(profile)
            .bind(refresh_token_hash.to_vec())
            .bind(refresh_expires_at)
            .execute(database.pool())
            .await
            .context("failed to create login session")?;
        } else {
            self.sessions.write().await.insert(
                session_id,
                StoredAuthSession {
                    id: session_id,
                    student: student.clone(),
                    roles: roles.clone(),
                    refresh_token_hash,
                    previous_refresh_token_hash: None,
                    expires_at: refresh_expires_at,
                    revoked_at: None,
                },
            );
        }

        Ok(CreatedAuthSession {
            session_id,
            student,
            roles,
            access_token: access.token,
            access_expires_at: access.expires_at,
            refresh_token,
            refresh_expires_at,
        })
    }

    pub async fn authenticate_access_token(
        &self,
        token: &str,
    ) -> anyhow::Result<Option<AuthPrincipal>> {
        let claims = match self.auth.verify_access_token(token) {
            Ok(claims) => claims,
            Err(_) => return Ok(None),
        };
        self.principal_for_claims(&claims).await
    }

    async fn principal_for_claims(
        &self,
        claims: &AccessClaims,
    ) -> anyhow::Result<Option<AuthPrincipal>> {
        if let Some(database) = &self.database {
            let row = sqlx::query(
                r#"SELECT s.profile, s.roles
                   FROM identity.auth_sessions s
                   JOIN platform.tenants t ON t.id = s.tenant_id
                   WHERE s.id = $1
                     AND s.user_id = $2
                     AND t.slug = $3
                     AND s.revoked_at IS NULL
                     AND s.expires_at > now()"#,
            )
            .bind(claims.sid)
            .bind(&claims.sub)
            .bind(&claims.tid)
            .fetch_optional(database.pool())
            .await
            .context("failed to validate login session")?;
            return row
                .map(|row| {
                    let student = serde_json::from_value(row.try_get("profile")?)
                        .context("deserialize login profile")?;
                    Ok(AuthPrincipal {
                        session_id: claims.sid,
                        student,
                        roles: row.try_get("roles")?,
                    })
                })
                .transpose();
        }

        let sessions = self.sessions.read().await;
        Ok(sessions.get(&claims.sid).and_then(|session| {
            (session.revoked_at.is_none()
                && session.expires_at > Utc::now()
                && session.student.id == claims.sub
                && session.student.tenant_id == claims.tid)
                .then(|| AuthPrincipal {
                    session_id: session.id,
                    student: session.student.clone(),
                    roles: session.roles.clone(),
                })
        }))
    }

    pub async fn refresh_session(&self, token: &str) -> anyhow::Result<RefreshSessionResult> {
        let supplied_hash = hash_refresh_token(token);
        if let Some(database) = &self.database {
            let mut transaction = database
                .pool()
                .begin()
                .await
                .context("start session refresh transaction")?;
            let row = sqlx::query(
                r#"SELECT s.id, t.slug AS tenant_slug, s.user_id, s.roles, s.profile,
                          s.refresh_token_hash, s.previous_refresh_token_hash, s.expires_at
                   FROM identity.auth_sessions s
                   JOIN platform.tenants t ON t.id = s.tenant_id
                   WHERE (s.refresh_token_hash = $1 OR s.previous_refresh_token_hash = $1)
                     AND s.revoked_at IS NULL
                   FOR UPDATE OF s"#,
            )
            .bind(supplied_hash.to_vec())
            .fetch_optional(&mut *transaction)
            .await
            .context("failed to resolve refresh session")?;
            let Some(row) = row else {
                transaction.commit().await.context("commit empty refresh")?;
                return Ok(RefreshSessionResult::Invalid);
            };
            let session_id: Uuid = row.try_get("id")?;
            let current_hash: Vec<u8> = row.try_get("refresh_token_hash")?;
            let expires_at: DateTime<Utc> = row.try_get("expires_at")?;
            if current_hash.as_slice() != supplied_hash || expires_at <= Utc::now() {
                sqlx::query("UPDATE identity.auth_sessions SET revoked_at = now() WHERE id = $1")
                    .bind(session_id)
                    .execute(&mut *transaction)
                    .await
                    .context("failed to revoke reused refresh session")?;
                transaction
                    .commit()
                    .await
                    .context("commit refresh reuse revocation")?;
                return Ok(if expires_at <= Utc::now() {
                    RefreshSessionResult::Invalid
                } else {
                    RefreshSessionResult::ReuseDetected
                });
            }

            let student: AuthStudent = serde_json::from_value(row.try_get("profile")?)
                .context("deserialize login profile")?;
            let roles: Vec<String> = row.try_get("roles")?;
            let new_refresh_token = generate_refresh_token();
            let new_refresh_hash = hash_refresh_token(&new_refresh_token);
            sqlx::query(
                r#"UPDATE identity.auth_sessions
                   SET previous_refresh_token_hash = refresh_token_hash,
                       refresh_token_hash = $2,
                       rotated_at = now(),
                       last_seen_at = now()
                   WHERE id = $1"#,
            )
            .bind(session_id)
            .bind(new_refresh_hash.to_vec())
            .execute(&mut *transaction)
            .await
            .context("failed to rotate refresh token")?;
            transaction
                .commit()
                .await
                .context("commit session refresh")?;
            let access = self
                .auth
                .issue_access_token(&student.id, &student.tenant_id, session_id, roles.clone())
                .context("failed to issue refreshed access token")?;
            return Ok(RefreshSessionResult::Rotated(Box::new(
                CreatedAuthSession {
                    session_id,
                    student,
                    roles,
                    access_token: access.token,
                    access_expires_at: access.expires_at,
                    refresh_token: new_refresh_token,
                    refresh_expires_at: expires_at,
                },
            )));
        }

        let mut sessions = self.sessions.write().await;
        let Some(session) = sessions.values_mut().find(|session| {
            session.refresh_token_hash == supplied_hash
                || session.previous_refresh_token_hash == Some(supplied_hash)
        }) else {
            return Ok(RefreshSessionResult::Invalid);
        };
        if session.revoked_at.is_some() || session.expires_at <= Utc::now() {
            session.revoked_at = Some(Utc::now());
            return Ok(RefreshSessionResult::Invalid);
        }
        if session.refresh_token_hash != supplied_hash {
            session.revoked_at = Some(Utc::now());
            return Ok(RefreshSessionResult::ReuseDetected);
        }
        let new_refresh_token = generate_refresh_token();
        let new_refresh_hash = hash_refresh_token(&new_refresh_token);
        session.previous_refresh_token_hash = Some(session.refresh_token_hash);
        session.refresh_token_hash = new_refresh_hash;
        let access = self
            .auth
            .issue_access_token(
                &session.student.id,
                &session.student.tenant_id,
                session.id,
                session.roles.clone(),
            )
            .context("failed to issue refreshed access token")?;
        Ok(RefreshSessionResult::Rotated(Box::new(
            CreatedAuthSession {
                session_id: session.id,
                student: session.student.clone(),
                roles: session.roles.clone(),
                access_token: access.token,
                access_expires_at: access.expires_at,
                refresh_token: new_refresh_token,
                refresh_expires_at: session.expires_at,
            },
        )))
    }

    pub async fn revoke_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        if let Some(database) = &self.database {
            sqlx::query(
                "UPDATE identity.auth_sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
            )
            .bind(session_id)
            .execute(database.pool())
            .await
            .context("failed to revoke login session")?;
            return Ok(());
        }
        if let Some(session) = self.sessions.write().await.get_mut(&session_id) {
            session.revoked_at = Some(Utc::now());
        }
        Ok(())
    }

    pub async fn revoke_refresh_token(&self, token: &str) -> anyhow::Result<()> {
        let token_hash = hash_refresh_token(token);
        if let Some(database) = &self.database {
            sqlx::query(
                r#"UPDATE identity.auth_sessions
                   SET revoked_at = now()
                   WHERE revoked_at IS NULL
                     AND (refresh_token_hash = $1 OR previous_refresh_token_hash = $1)"#,
            )
            .bind(token_hash.to_vec())
            .execute(database.pool())
            .await
            .context("failed to revoke refresh session")?;
            return Ok(());
        }
        if let Some(session) = self.sessions.write().await.values_mut().find(|session| {
            session.refresh_token_hash == token_hash
                || session.previous_refresh_token_hash == Some(token_hash)
        }) {
            session.revoked_at = Some(Utc::now());
        }
        Ok(())
    }

    /// Mints a one-minute token for the realtime WebSocket handshake.
    pub fn issue_realtime_token(
        &self,
        principal: &AuthPrincipal,
    ) -> anyhow::Result<supercampus_authn::IssuedAccessToken> {
        self.auth
            .issue_access_token_with_ttl(
                &principal.student.id,
                &principal.student.tenant_id,
                principal.session_id,
                principal.roles.clone(),
                REALTIME_TOKEN_TTL_SECONDS,
            )
            .context("failed to issue realtime token")
    }

    /// Resolves the navigation a caller may see.
    ///
    /// Sections live in `platform.navigation_sections` per tenant, so a tenant
    /// administrator decides which parts of the workspace exist and which grants reveal
    /// them. Visibility is recomputed from live effective grants on every call, so a
    /// permission change takes effect without a new token or a redeploy.
    pub async fn navigation(
        &self,
        tenant_slug: &str,
        access: &EffectiveAccess,
    ) -> anyhow::Result<Value> {
        let mut sections: Vec<NavigationSection> = Vec::new();
        if let Some(database) = &self.database {
            let rows = sqlx::query(
                r#"SELECT section.section_key, section.kind, section.label, section.route,
                          section.icon, section.required_permissions, section.module_key,
                          section.always_visible
                   FROM platform.navigation_sections section
                   JOIN platform.tenants tenant ON tenant.id = section.tenant_id
                   WHERE tenant.slug = $1 AND section.active
                   ORDER BY section.kind, section.sort_order, section.label"#,
            )
            .bind(tenant_slug)
            .fetch_all(database.pool())
            .await
            .context("failed to load navigation sections")?;
            for row in &rows {
                sections.push(NavigationSection {
                    key: row.try_get("section_key")?,
                    kind: row.try_get("kind")?,
                    label: row.try_get("label")?,
                    route: row.try_get("route")?,
                    icon: row.try_get("icon")?,
                    required: row.try_get("required_permissions")?,
                    module_key: row.try_get("module_key")?,
                    always_visible: row.try_get("always_visible")?,
                });
            }
        }
        // An institution created after the navigation migration has no rows yet. Fall
        // back to the platform defaults so a new tenant is never left without a menu.
        if sections.is_empty() {
            sections = default_navigation_sections();
        }

        let mut workspace = Vec::new();
        let mut settings = Vec::new();
        for section in &sections {
            // The Settings entry is resolved after its children are known.
            if section.kind == "workspace" && section.key == "settings" {
                continue;
            }
            if !section_is_visible(
                access,
                &section.required,
                section.module_key.as_deref(),
                section.always_visible,
            ) {
                continue;
            }
            let entry = json!({
                "key": section.key,
                "label": section.label,
                "route": section.route,
                "icon": section.icon,
            });
            if section.kind == "settings" {
                settings.push(entry);
            } else {
                workspace.push(entry);
            }
        }

        // Settings only appears when at least one settings child is reachable.
        if let Some(section) = sections
            .iter()
            .find(|section| section.kind == "workspace" && section.key == "settings")
            && !settings.is_empty()
        {
            workspace.push(json!({
                "key": section.key,
                "label": section.label,
                "route": section.route,
                "icon": section.icon,
            }));
        }

        Ok(json!({
            "workspace": workspace,
            "settings": settings,
            "permissions": access.permissions,
            "roles": access.roles,
            "scopes": access.scopes,
        }))
    }

    /// Starts a password reset.
    ///
    /// Always succeeds from the caller's perspective. An unknown address, an inactive
    /// account, and a throttled account are indistinguishable in the response so the
    /// endpoint cannot be used to enumerate registered emails.
    pub async fn request_password_reset(&self, email: &str, base_url: &str) -> anyhow::Result<()> {
        let email = email.trim().to_ascii_lowercase();
        let Some(database) = &self.database else {
            tracing::warn!("password reset requested without PostgreSQL configured");
            return Ok(());
        };

        let user: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, display_name FROM identity.users WHERE email = $1 AND active",
        )
        .bind(&email)
        .fetch_optional(database.pool())
        .await
        .context("failed to look up the reset account")?;

        let Some((user_id, display_name)) = user else {
            // Spend no further work, but do not tell the caller.
            tracing::info!("password reset requested for an unknown or inactive address");
            return Ok(());
        };

        let recent: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM identity.password_reset_tokens
               WHERE user_id = $1 AND created_at > now() - ($2 || ' minutes')::interval"#,
        )
        .bind(user_id)
        .bind(PASSWORD_RESET_THROTTLE_MINUTES.to_string())
        .fetch_one(database.pool())
        .await
        .context("failed to count recent reset requests")?;

        if recent >= PASSWORD_RESET_MAX_REQUESTS {
            tracing::warn!(%user_id, "password reset throttled");
            return Ok(());
        }

        // The raw token goes to the user; only its digest is stored.
        let token = generate_refresh_token();
        let token_hash = hash_refresh_token(&token);
        let expires_at = Utc::now() + chrono::Duration::minutes(PASSWORD_RESET_TTL_MINUTES);

        sqlx::query(
            r#"INSERT INTO identity.password_reset_tokens (user_id, token_hash, expires_at)
               VALUES ($1, $2, $3)"#,
        )
        .bind(user_id)
        .bind(token_hash.to_vec())
        .bind(expires_at)
        .execute(database.pool())
        .await
        .context("failed to store the reset token")?;

        let reset_url = format!(
            "{}/reset-password?token={token}",
            base_url.trim_end_matches('/')
        );
        let message = password_reset_email(&email, &display_name, &reset_url);

        // Delivery failure must not surface a different response to the caller, otherwise
        // the timing and status differences leak which addresses exist.
        if let Err(error) = self.mailer.send(message).await {
            tracing::error!(error = ?error, %user_id, "failed to deliver the password reset email");
        }
        Ok(())
    }

    /// Completes a password reset. Returns `false` when the token is unknown, expired,
    /// or already used.
    pub async fn reset_password(&self, token: &str, new_password: &str) -> anyhow::Result<bool> {
        let database = self.database.as_ref().context("PostgreSQL is required")?;
        let token_hash = hash_refresh_token(token);

        let mut transaction = database.pool().begin().await?;

        // Claim the token first. The UPDATE ... RETURNING is atomic, so two concurrent
        // submissions of the same link cannot both proceed.
        let claimed: Option<Uuid> = sqlx::query_scalar(
            r#"UPDATE identity.password_reset_tokens
               SET consumed_at = now()
               WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now()
               RETURNING user_id"#,
        )
        .bind(token_hash.to_vec())
        .fetch_optional(&mut *transaction)
        .await
        .context("failed to claim the reset token")?;

        let Some(user_id) = claimed else {
            transaction.rollback().await?;
            return Ok(false);
        };

        let updated = sqlx::query(
            r#"UPDATE identity.users
               SET password_hash = crypt($2, gen_salt('bf', 12)), updated_at = now()
               WHERE id = $1 AND active"#,
        )
        .bind(user_id)
        .bind(new_password)
        .execute(&mut *transaction)
        .await
        .context("failed to update the password")?;

        if updated.rows_affected() == 0 {
            // The account was deactivated between the request and the reset.
            transaction.rollback().await?;
            return Ok(false);
        }

        // Any other outstanding link for this account is now void.
        sqlx::query(
            r#"UPDATE identity.password_reset_tokens
               SET consumed_at = now()
               WHERE user_id = $1 AND consumed_at IS NULL"#,
        )
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .context("failed to invalidate outstanding reset tokens")?;

        // A password change signs out every existing device.
        sqlx::query(
            r#"UPDATE identity.auth_sessions
               SET revoked_at = now()
               WHERE user_id = $1 AND revoked_at IS NULL"#,
        )
        .bind(user_id.to_string())
        .execute(&mut *transaction)
        .await
        .context("failed to revoke sessions after the password change")?;

        transaction.commit().await?;
        tracing::info!(%user_id, "password reset completed and sessions revoked");
        Ok(true)
    }

    pub async fn app_state(
        &self,
        tenant_id: &str,
        student_id: &str,
    ) -> anyhow::Result<StoredAppState> {
        if self.database.is_some() {
            let database = self.tenant_database(tenant_id).await?;
            let row = sqlx::query(
                r#"SELECT s.state, s.version, s.updated_at
                   FROM identity.ui_states s
                   JOIN platform.tenants t ON t.id = s.tenant_id
                   WHERE t.slug = $1 AND s.user_id = $2"#,
            )
            .bind(tenant_id)
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
            .get(&app_state_key(tenant_id, student_id))
            .cloned()
            .unwrap_or_else(default_app_state))
    }

    pub async fn save_app_state(
        &self,
        tenant_id: String,
        student_id: String,
        state: Value,
    ) -> anyhow::Result<StoredAppState> {
        if self.database.is_some() {
            let database = self.tenant_database(&tenant_id).await?;
            let tenant_uuid = ensure_tenant(&database, &tenant_id).await?;
            let row = sqlx::query(
                r#"INSERT INTO identity.ui_states (tenant_id, user_id, state, version)
                   VALUES ($1, $2, $3, 1)
                   ON CONFLICT (tenant_id, user_id) DO UPDATE
                   SET state = EXCLUDED.state,
                       version = EXCLUDED.version + 1,
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
        let key = app_state_key(&tenant_id, &student_id);
        let version = states.get(&key).map_or(1, |document| document.version + 1);
        let document = StoredAppState {
            state,
            version,
            updated_at: Utc::now(),
        };
        states.insert(key, document.clone());
        Ok(document)
    }
}

fn authorization_role_row(row: &PgRow, permissions: Value) -> anyhow::Result<Value> {
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?,
        "key": row.try_get::<String, _>("role_key")?,
        "name": row.try_get::<String, _>("name")?,
        "team": row.try_get::<String, _>("team")?,
        "scope": row.try_get::<String, _>("scope_description")?,
        "protected": row.try_get::<bool, _>("protected")?,
        "active": row.try_get::<bool, _>("active")?,
        "permissions": permissions,
    }))
}

async fn replace_user_roles(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    user_id: Uuid,
    role_ids: &[Uuid],
    actor_id: &str,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM authz.user_roles WHERE tenant_id = $1 AND user_id = $2")
        .bind(tenant_id)
        .bind(user_id)
        .execute(&mut **transaction)
        .await?;
    for role_id in role_ids {
        sqlx::query(
            r#"INSERT INTO authz.user_roles
               (tenant_id, user_id, role_id, assigned_by)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(role_id)
        .bind(actor_id)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn ensure_tenant(database: &Database, tenant_slug: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar(
        r#"INSERT INTO platform.tenants (slug, code, name)
           VALUES ($1, upper(replace($1, '-', '_')), $2)
           ON CONFLICT (slug) DO UPDATE SET updated_at = now()
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

fn row_to_tenant(row: &PgRow) -> anyhow::Result<TenantSummary> {
    Ok(TenantSummary {
        id: row.try_get("slug")?,
        code: row.try_get("code")?,
        name: row.try_get("name")?,
        city: row
            .try_get::<Option<String>, _>("city")?
            .unwrap_or_default(),
    })
}

fn identity_key(email: &str, tenant_id: &str) -> String {
    format!("{}:{}", email.trim().to_ascii_lowercase(), tenant_id.trim())
}

fn record_key(tenant_id: &str, module_key: &str, id: Uuid) -> String {
    format!("{tenant_id}:{module_key}:{id}")
}

fn configuration_key(tenant_id: &str, namespace: &str) -> String {
    format!("{tenant_id}:{namespace}")
}

fn workflow_namespace(module: &str, feature: &str) -> String {
    format!("workflows.{module}.{feature}")
}

fn seed_workflow_definition(
    tenant_id: &str,
    module: &str,
    feature: &str,
) -> Option<WorkflowDefinition> {
    if module != "gatepass" || feature != "outpass" {
        return None;
    }
    match tenant_id {
        "tenant-a" | "college-1" => Some(college_one_gatepass_workflow(tenant_id)),
        "tenant-b" | "college-2" | "tenant-local" => Some(college_two_gatepass_workflow(tenant_id)),
        _ => None,
    }
}

fn college_one_gatepass_workflow(tenant_id: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        tenant_id: tenant_id.to_owned(),
        module: "gatepass".into(),
        feature: "outpass".into(),
        version: 1,
        initial_state: "draft".into(),
        terminal_states: vec!["rejected".into(), "completed".into()],
        states: vec![
            workflow_state("draft", "Draft", WorkflowStateStatus::Draft),
            workflow_state(
                "submitted",
                "Student submitted",
                WorkflowStateStatus::Pending,
            ),
            workflow_state(
                "parent_approved",
                "Parent approved",
                WorkflowStateStatus::Approved,
            ),
            workflow_state(
                "warden_approved",
                "Warden approved",
                WorkflowStateStatus::Approved,
            ),
            workflow_state(
                "security_verified",
                "Security verified",
                WorkflowStateStatus::Completed,
            ),
            workflow_state("rejected", "Rejected", WorkflowStateStatus::Rejected),
            workflow_state(
                "completed",
                "Exit completed",
                WorkflowStateStatus::Completed,
            ),
        ],
        transitions: vec![
            workflow_transition(
                "draft",
                "submitted",
                "submit",
                "gatepass.outpass.create",
                None,
                "Submit request",
            ),
            workflow_transition(
                "submitted",
                "parent_approved",
                "approve",
                "gatepass.outpass.approve",
                Some("parent"),
                "Parent approve",
            ),
            workflow_transition(
                "parent_approved",
                "warden_approved",
                "approve",
                "gatepass.outpass.approve",
                Some("warden"),
                "Warden approve",
            ),
            workflow_transition(
                "warden_approved",
                "security_verified",
                "verify",
                "gatepass.outpass.verify",
                Some("security"),
                "Security verify",
            ),
            workflow_transition(
                "security_verified",
                "completed",
                "complete",
                "gatepass.outpass.update",
                Some("security"),
                "Complete exit",
            ),
            workflow_transition(
                "submitted",
                "rejected",
                "reject",
                "gatepass.outpass.reject",
                None,
                "Reject",
            ),
            workflow_transition(
                "parent_approved",
                "rejected",
                "reject",
                "gatepass.outpass.reject",
                None,
                "Reject",
            ),
        ],
    }
}

fn college_two_gatepass_workflow(tenant_id: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        tenant_id: tenant_id.to_owned(),
        module: "gatepass".into(),
        feature: "outpass".into(),
        version: 1,
        initial_state: "draft".into(),
        terminal_states: vec!["rejected".into(), "completed".into()],
        states: vec![
            workflow_state("draft", "Draft", WorkflowStateStatus::Draft),
            workflow_state(
                "submitted",
                "Student submitted",
                WorkflowStateStatus::Pending,
            ),
            workflow_state(
                "warden_approved",
                "Warden approved",
                WorkflowStateStatus::Approved,
            ),
            workflow_state(
                "security_verified",
                "Security verified",
                WorkflowStateStatus::Completed,
            ),
            workflow_state("rejected", "Rejected", WorkflowStateStatus::Rejected),
            workflow_state(
                "completed",
                "Exit completed",
                WorkflowStateStatus::Completed,
            ),
        ],
        transitions: vec![
            workflow_transition(
                "draft",
                "submitted",
                "submit",
                "gatepass.outpass.create",
                None,
                "Submit request",
            ),
            workflow_transition(
                "submitted",
                "warden_approved",
                "approve",
                "gatepass.outpass.approve",
                Some("warden"),
                "Warden approve",
            ),
            workflow_transition(
                "warden_approved",
                "security_verified",
                "verify",
                "gatepass.outpass.verify",
                Some("security"),
                "Security verify",
            ),
            workflow_transition(
                "security_verified",
                "completed",
                "complete",
                "gatepass.outpass.update",
                Some("security"),
                "Complete exit",
            ),
            workflow_transition(
                "submitted",
                "rejected",
                "reject",
                "gatepass.outpass.reject",
                None,
                "Reject",
            ),
        ],
    }
}

fn workflow_state(id: &str, label: &str, status: WorkflowStateStatus) -> WorkflowState {
    WorkflowState {
        id: id.into(),
        label: label.into(),
        status,
    }
}

fn workflow_transition(
    from: &str,
    to: &str,
    action: &str,
    required_permission: &str,
    required_role: Option<&str>,
    label: &str,
) -> WorkflowTransition {
    WorkflowTransition {
        from: from.into(),
        to: to.into(),
        action: action.into(),
        required_permission: required_permission.into(),
        required_role: required_role.map(str::to_owned),
        label: label.into(),
    }
}

struct NavigationSection {
    key: String,
    kind: String,
    label: String,
    route: Option<String>,
    icon: Option<String>,
    required: Vec<String>,
    module_key: Option<String>,
    always_visible: bool,
}

type DefaultNavigationSection = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<&'static str>,
    bool,
);

/// Platform defaults, mirroring migration 0019.
///
/// Used only when an institution has no navigation rows of its own, which happens for
/// tenants provisioned after that migration ran.
fn default_navigation_sections() -> Vec<NavigationSection> {
    const DEFAULTS: &[DefaultNavigationSection] = &[
        (
            "dashboard",
            "workspace",
            "Overview",
            "LayoutDashboard",
            &["crm.dashboard.read"],
            None,
            false,
        ),
        (
            "pipeline",
            "workspace",
            "Lead",
            "Kanban",
            &["crm.leads.read"],
            None,
            false,
        ),
        (
            "admissions",
            "workspace",
            "Admissions",
            "ClipboardList",
            &["crm.erp.handoff"],
            Some("admissions"),
            false,
        ),
        (
            "application-desk",
            "workspace",
            "Application Desk",
            "IdCard",
            &[],
            Some("application-desk"),
            false,
        ),
        (
            "students",
            "workspace",
            "Students",
            "Users",
            &[],
            Some("students"),
            false,
        ),
        (
            "academics",
            "workspace",
            "Academics",
            "ListChecks",
            &[],
            Some("academics"),
            false,
        ),
        (
            "fees",
            "workspace",
            "Fees & Finance",
            "Database",
            &[],
            Some("fees"),
            false,
        ),
        (
            "erp",
            "workspace",
            "ERP Services",
            "Layers",
            &[],
            Some("erp"),
            false,
        ),
        (
            "reports",
            "workspace",
            "Reports & BI",
            "BarChart3",
            &["crm.reports.read"],
            None,
            false,
        ),
        (
            "users",
            "workspace",
            "Users & Roles",
            "UserCog",
            &[
                "authorization.users.read",
                "authorization.roles.read",
                "authorization.permissions.read",
            ],
            None,
            false,
        ),
        (
            "settings",
            "workspace",
            "Settings",
            "Settings",
            &[],
            None,
            false,
        ),
        ("account", "settings", "Account", "UserCog", &[], None, true),
        (
            "access",
            "settings",
            "Access Control",
            "ShieldCheck",
            &[
                "authorization.permissions.read",
                "authorization.roles.read",
                "authorization.users.read",
            ],
            None,
            false,
        ),
        (
            "forms",
            "settings",
            "Form Builders",
            "ClipboardList",
            &["crm.forms.read"],
            None,
            false,
        ),
        (
            "workflows",
            "settings",
            "Workflow Studio",
            "Workflow",
            &["crm.configuration.read"],
            None,
            false,
        ),
        (
            "theme",
            "settings",
            "Theme",
            "Palette",
            &["platform.configuration.update"],
            None,
            false,
        ),
    ];
    DEFAULTS
        .iter()
        .map(
            |(key, kind, label, icon, required, module_key, always_visible)| NavigationSection {
                key: (*key).into(),
                kind: (*kind).into(),
                label: (*label).into(),
                route: Some("/dashboard/admissions".into()),
                icon: Some((*icon).into()),
                required: required.iter().map(|value| (*value).to_string()).collect(),
                module_key: module_key.map(str::to_owned),
                always_visible: *always_visible,
            },
        )
        .collect()
}

/// ANY-of visibility: a full-tenant grant, an explicitly listed permission, or any
/// permission inside the section's module reveals it.
fn section_is_visible(
    access: &EffectiveAccess,
    required: &[String],
    module_key: Option<&str>,
    always_visible: bool,
) -> bool {
    if always_visible || access.allows("*") {
        return true;
    }
    if required.iter().any(|permission| access.allows(permission)) {
        return true;
    }
    match module_key {
        Some(module_key) => {
            let prefix = format!("{module_key}.");
            access
                .permissions
                .iter()
                .any(|permission| permission.starts_with(&prefix))
        }
        None => false,
    }
}

fn password_reset_email(email: &str, display_name: &str, reset_url: &str) -> EmailMessage {
    let name = display_name.trim();
    let greeting = if name.is_empty() { "Hello" } else { name };
    let text_body = format!(
        "{greeting},\n\n\
         We received a request to reset the password for your SuperCampus account.\n\n\
         Open this link to choose a new password:\n{reset_url}\n\n\
         The link expires in {PASSWORD_RESET_TTL_MINUTES} minutes and can be used once.\n\n\
         If you did not request this, you can ignore this email. Your password stays unchanged.\n\n\
         SuperCampus"
    );
    let html_body = format!(
        "<p>{greeting},</p>\
         <p>We received a request to reset the password for your SuperCampus account.</p>\
         <p><a href=\"{reset_url}\">Choose a new password</a></p>\
         <p>The link expires in {PASSWORD_RESET_TTL_MINUTES} minutes and can be used once.</p>\
         <p>If you did not request this, you can ignore this email. Your password stays unchanged.</p>\
         <p>SuperCampus</p>"
    );
    EmailMessage {
        to: email.to_owned(),
        subject: "Reset your SuperCampus password".into(),
        text_body,
        html_body: Some(html_body),
    }
}

fn app_state_key(tenant_id: &str, user_id: &str) -> String {
    format!("{tenant_id}:{user_id}")
}

fn identity_student(input: IdentityStudentInput) -> AuthStudent {
    let IdentityStudentInput {
        id,
        email,
        name,
        tenant,
        role,
        profile,
    } = input;
    let tenant_name = tenant.name.clone();
    let profile_string = |key: &str, fallback: &str| {
        profile
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };
    let access = profile
        .get("access")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    AuthStudent {
        id,
        tenant_id: tenant.id.clone(),
        email,
        initials: initials(&name),
        name,
        role,
        team: profile_string("team", ""),
        access,
        roll: profile_string("roll", ""),
        college: profile_string("college", &tenant_name),
        dept: profile_string("dept", ""),
        year: profile_string("year", ""),
        full_college: profile_string("fullCollege", &tenant_name),
        tenant,
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn display_name_from_email(email: &str) -> String {
    email
        .split('@')
        .next()
        .unwrap_or("User")
        .split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn environment_flag(name: &str) -> bool {
    std::env::var(name)
        .is_ok_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

async fn seed_identity(
    database: &Database,
    tenant_id: Uuid,
    identity: &SeedIdentity<'_>,
) -> anyhow::Result<()> {
    let email = identity.email.trim().to_ascii_lowercase();
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO identity.users
           (email, password_hash, display_name, initials, account_type)
           VALUES ($1, crypt($2, gen_salt('bf', 12)), $3, $4, $5)
           ON CONFLICT (email) DO UPDATE SET password_hash = crypt($2, gen_salt('bf', 12)),
               display_name = EXCLUDED.display_name, initials = EXCLUDED.initials,
               account_type = EXCLUDED.account_type, active = true, updated_at = now()
           RETURNING id"#,
    )
    .bind(email)
    .bind(identity.password)
    .bind(identity.display_name)
    .bind(initials(identity.display_name))
    .bind(identity.account_type)
    .fetch_one(database.pool())
    .await
    .context("failed to seed test identity")?;
    let profile = json!({ "team": identity.team, "access": ["crm"] });
    sqlx::query(
        r#"INSERT INTO identity.tenant_memberships (tenant_id, user_id, roles, is_primary, profile)
           VALUES ($1, $2, $3, true, $4)
           ON CONFLICT (tenant_id, user_id) DO UPDATE SET roles = EXCLUDED.roles,
               active = true, is_primary = true, profile = EXCLUDED.profile, updated_at = now()"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(vec![identity.role.to_owned()])
    .bind(profile)
    .execute(database.pool())
    .await
    .context("failed to seed test tenant membership")?;

    let protected = identity.role == "tenant_admin";
    let role_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO authz.roles
           (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
           VALUES ($1, $2, $3, $4, $5, $6, 'test-seeder', 'test-seeder')
           ON CONFLICT (tenant_id, role_key) DO UPDATE SET name = EXCLUDED.name,
               team = EXCLUDED.team, active = true, updated_by = 'test-seeder', updated_at = now()
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(identity.role)
    .bind(humanize_identifier(identity.role))
    .bind(identity.team)
    .bind("Testing role configured from environment")
    .bind(protected)
    .fetch_one(database.pool())
    .await
    .context("failed to seed test authorization role")?;
    if protected {
        sqlx::query(
            r#"INSERT INTO authz.role_permissions
               (tenant_id, role_id, permission_key, scope, granted_by)
               VALUES ($1, $2, '*', 'all', 'test-seeder')
               ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(role_id)
        .execute(database.pool())
        .await
        .context("failed to grant tenant admin test access")?;
    }
    sqlx::query("DELETE FROM authz.user_roles WHERE tenant_id = $1 AND user_id = $2")
        .bind(tenant_id)
        .bind(user_id)
        .execute(database.pool())
        .await
        .context("failed to reset test user roles")?;
    sqlx::query(
        r#"INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
           VALUES ($1, $2, $3, 'test-seeder')"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(role_id)
    .execute(database.pool())
    .await
    .context("failed to assign test authorization role")?;
    Ok(())
}

fn access_scope_rank(scope: &str) -> u8 {
    match scope {
        "all" => 3,
        "assigned" => 2,
        "own" => 1,
        _ => 0,
    }
}

fn humanize_identifier(value: &str) -> String {
    value
        .split(['_', '-', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_app_state() -> StoredAppState {
    StoredAppState {
        state: json!({
            "persona": "dayscholar",
            "gp": { "status": "none", "type": null, "early": false, "step": 0 },
            "paid": { "tuition": false, "hostel": false, "transport": false, "exam": false },
            "pay": { "comp": null, "step": 0, "plan": null, "mode": null },
            "refunds": {},
            "condonation": "none",
            "examReg": 0,
            "reval": {},
            "asg": { "a3": "none" },
            "changeNotice": false,
            "mess": false,
            "hostelLeave": 0,
            "hostelTickets": [],
            "tripStep": 0,
            "breakdown": false,
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
