use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use supercampus_authn::{AccessClaims, AuthService, generate_refresh_token, hash_refresh_token};
use supercampus_database::{Database, TenantDatabaseManager};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::models::{
    AssignUserRolesRequest, AuthStudent, ConfigurationDocument, CreateAuthorizationRoleRequest,
    CreateTenantUserRequest, DynamicRecord, ModuleDescriptor, PermissionGrantRequest,
    ServiceDescriptor, StoredAppState, TenantSummary, UpdateAuthorizationRoleRequest,
};

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
    pub fn allows(&self, permission: &str) -> bool {
        self.permissions
            .iter()
            .any(|value| value == "*" || value == permission)
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
                    city: row.try_get("city")?,
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
        if let Some(database) = &self.database {
            let rows = sqlx::query(
                r#"SELECT role.role_key, role_grant.permission_key, role_grant.scope
                   FROM platform.tenants tenant
                   JOIN authz.user_roles user_role ON user_role.tenant_id = tenant.id
                   JOIN authz.roles role ON role.id = user_role.role_id
                       AND role.tenant_id = tenant.id AND role.active
                   LEFT JOIN authz.role_permissions role_grant ON role_grant.tenant_id = tenant.id
                       AND role_grant.role_id = role.id
                   LEFT JOIN authz.permission_definitions permission
                       ON permission.tenant_id = tenant.id
                       AND permission.permission_key = role_grant.permission_key
                       AND permission.active
                   WHERE tenant.slug = $1 AND user_role.user_id = $2::uuid
                   ORDER BY role.name, role_grant.permission_key"#,
            )
            .bind(tenant_slug)
            .bind(user_id)
            .fetch_all(database.pool())
            .await
            .context("failed to load effective tenant access")?;

            let mut roles = Vec::new();
            let mut permissions = Vec::new();
            let mut scopes: HashMap<String, String> = HashMap::new();
            for row in rows {
                let role_key: String = row.try_get("role_key")?;
                if !roles.contains(&role_key) {
                    roles.push(role_key);
                }
                let permission_key: Option<String> = row.try_get("permission_key")?;
                let scope: Option<String> = row.try_get("scope")?;
                if let Some(permission_key) = permission_key {
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
        let temporary_password = request
            .temporary_password
            .clone()
            .unwrap_or_else(|| format!("SC-{}!Aa9", Uuid::new_v4().simple()));
        let existing_user: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM identity.users WHERE email = $1")
                .bind(&email)
                .fetch_optional(database.pool())
                .await?;
        let created = existing_user.is_none();
        let user_id = if let Some(user_id) = existing_user {
            user_id
        } else {
            sqlx::query_scalar(
                r#"INSERT INTO identity.users
                   (email, password_hash, display_name, initials, account_type)
                   VALUES ($1, crypt($2, gen_salt('bf', 12)), $3, $4, 'staff')
                   RETURNING id"#,
            )
            .bind(&email)
            .bind(&temporary_password)
            .bind(request.name.trim())
            .bind(initials(request.name.trim()))
            .fetch_one(database.pool())
            .await?
        };

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
        let membership = sqlx::query(
            r#"INSERT INTO identity.tenant_memberships
               (tenant_id, user_id, roles, is_primary, profile)
               VALUES ($1, $2, $3, true, '{}'::jsonb)
               ON CONFLICT (tenant_id, user_id) DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&role_keys)
        .execute(&mut *transaction)
        .await?;
        if membership.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
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
            "temporaryPassword": created.then_some(temporary_password),
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
        let tenant_slug = required_environment("TEST_TENANT_SLUG")?;
        let tenant_name = required_environment("TEST_TENANT_NAME")?;
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
            let (Ok(email), Ok(password)) =
                (std::env::var(&email_key), std::env::var(&password_key))
            else {
                continue;
            };
            if email.trim().is_empty() || password.len() < 12 {
                bail!(
                    "{email_key} must be set and {password_key} must contain at least 12 characters"
                );
            }
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

fn row_to_tenant(row: &PgRow) -> anyhow::Result<TenantSummary> {
    Ok(TenantSummary {
        id: row.try_get("slug")?,
        code: row.try_get("code")?,
        name: row.try_get("name")?,
        city: row.try_get("city")?,
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

fn required_environment(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name)
        .with_context(|| format!("{name} is required when SEED_TEST_USERS=true"))?;
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(value)
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
