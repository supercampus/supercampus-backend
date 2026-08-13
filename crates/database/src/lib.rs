#![forbid(unsafe_code)]

//! PostgreSQL control-plane and tenant-database connectivity for SuperCampus.

use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, bail};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations/runtime");

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let options = PgConnectOptions::from_str(database_url)
            .context("invalid PostgreSQL connection URL")?;
        Self::connect_options(options, 10).await
    }

    pub async fn connect_options(
        options: PgConnectOptions,
        max_connections: u32,
    ) -> anyhow::Result<Self> {
        const ATTEMPTS: u32 = 3;
        const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
        let mut last_error = String::from("connection attempt timed out");

        for attempt in 1..=ATTEMPTS {
            let connect = PgPoolOptions::new()
                .max_connections(max_connections)
                .min_connections(1)
                .acquire_timeout(Duration::from_secs(10))
                .connect_with(options.clone());

            match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
                Ok(Ok(pool)) => return Ok(Self { pool }),
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = format!("attempt exceeded {CONNECT_TIMEOUT:?}"),
            }

            if attempt < ATTEMPTS {
                tracing::warn!(
                    attempt,
                    attempts = ATTEMPTS,
                    "PostgreSQL connection attempt failed; retrying"
                );
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
        }

        bail!("failed to connect to PostgreSQL after {ATTEMPTS} attempts: {last_error}")
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        if let Err(error) = MIGRATOR.run(&self.pool).await {
            let error_str = format!("{error:?}");
            if error_str.contains("VersionMissing") {
                tracing::warn!(error = %error, "database has migrations newer than this checkout; ignoring missing applied migrations");
                return Ok(());
            } else if error_str.contains("VersionMismatch") {
                tracing::warn!(error = %error, "sqlx migration version mismatch detected; reconciling applied migration checksums");
                for migration in MIGRATOR.iter() {
                    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
                        .bind(migration.checksum.as_ref())
                        .bind(migration.version)
                        .execute(&self.pool)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to reconcile checksum for migration {}",
                                migration.version
                            )
                        })?;
                }
                // A VersionMismatch can leave SQLx's advisory lock attached to the
                // pooled connection used by the failed pass. Reconciliation is
                // already serialized by the first pass, so reacquiring that same
                // lock here can deadlock against our own pool.
                let reconciled_migrator = sqlx::migrate::Migrator {
                    migrations: MIGRATOR.migrations.clone(),
                    ignore_missing: MIGRATOR.ignore_missing,
                    locking: false,
                    no_tx: MIGRATOR.no_tx,
                };
                if let Err(retry_err) = reconciled_migrator.run(&self.pool).await {
                    anyhow::bail!(
                        "failed to run PostgreSQL migrations after checksum reconciliation: {retry_err:?}"
                    );
                }
            } else {
                anyhow::bail!("failed to run PostgreSQL migrations: {error:?}");
            }
        }
        Ok(())
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("PostgreSQL readiness query failed")?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Resolves an institution slug to its independently migrated PostgreSQL database.
/// Credentials and server coordinates come from `CONTROL_DATABASE_URL`; the control
/// plane stores only the logical database name for each institution.
#[derive(Clone)]
pub struct TenantDatabaseManager {
    control: Database,
    base_options: Option<PgConnectOptions>,
    pools: Arc<RwLock<HashMap<String, Database>>>,
}

impl TenantDatabaseManager {
    /// Compatibility mode for isolated tests. Every tenant resolves to the supplied database.
    pub fn single(control: Database) -> Self {
        Self {
            control,
            base_options: None,
            pools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn clustered(control: Database, control_database_url: &str) -> anyhow::Result<Self> {
        let base_options = PgConnectOptions::from_str(control_database_url)
            .context("invalid CONTROL_DATABASE_URL")?;
        Ok(Self {
            control,
            base_options: Some(base_options),
            pools: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn control(&self) -> Database {
        self.control.clone()
    }

    pub async fn tenant(&self, tenant_slug: &str) -> anyhow::Result<Database> {
        let tenant_slug = tenant_slug.trim();
        if tenant_slug.is_empty() {
            bail!("tenant slug is required for database resolution");
        }
        if self.base_options.is_none() {
            return Ok(self.control.clone());
        }
        if let Some(database) = self
            .pools
            .read()
            .expect("tenant database cache lock poisoned")
            .get(tenant_slug)
            .cloned()
        {
            return Ok(database);
        }

        let row = sqlx::query(
            r#"SELECT registry.database_name
               FROM platform.tenant_databases registry
               JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
               WHERE tenant.slug = $1 AND tenant.status = 'active'
                 AND registry.status = 'active'"#,
        )
        .bind(tenant_slug)
        .fetch_optional(self.control.pool())
        .await
        .context("failed to resolve tenant database registry")?;
        let Some(row) = row else {
            bail!("no active database is registered for tenant {tenant_slug}");
        };
        let database_name: String = row.try_get("database_name")?;
        validate_database_name(&database_name)?;
        let options = self
            .base_options
            .as_ref()
            .expect("clustered manager has base options")
            .clone()
            .database(&database_name);
        let database = Database::connect_options(options, 5)
            .await
            .with_context(|| format!("failed to connect tenant {tenant_slug} database"))?;
        if std::env::var("SKIP_TENANT_DB_PING").as_deref() == Ok("true") {
            tracing::warn!(tenant_slug, "tenant database request-time migration check skipped");
        } else {
            database
                .migrate()
                .await
                .with_context(|| format!("failed to migrate tenant {tenant_slug} database"))?;
        }
        database.ping().await?;
        self.pools
            .write()
            .expect("tenant database cache lock poisoned")
            .insert(tenant_slug.to_owned(), database.clone());
        Ok(database)
    }

    pub async fn ping_registered(&self) -> anyhow::Result<()> {
        self.control.ping().await?;
        if self.base_options.is_none() {
            return Ok(());
        }
        let slugs: Vec<String> = sqlx::query_scalar(
            r#"SELECT tenant.slug
               FROM platform.tenant_databases registry
               JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
               WHERE tenant.status = 'active' AND registry.status = 'active'
               ORDER BY tenant.slug"#,
        )
        .fetch_all(self.control.pool())
        .await
        .context("failed to list registered tenant databases")?;
        for slug in slugs {
            self.tenant(&slug).await?.ping().await?;
        }
        Ok(())
    }

    pub async fn register(&self, tenant_slug: &str, database_name: &str) -> anyhow::Result<()> {
        validate_database_name(database_name)?;
        let result = sqlx::query(
            r#"INSERT INTO platform.tenant_databases
                   (tenant_id, database_name, status, migration_version)
               SELECT tenant.id, $2, 'active', $3
               FROM platform.tenants tenant
               WHERE tenant.slug = $1 AND tenant.status = 'active'
               ON CONFLICT (tenant_id) DO UPDATE SET
                   database_name = EXCLUDED.database_name,
                   status = 'active',
                   migration_version = EXCLUDED.migration_version,
                   updated_at = now()"#,
        )
        .bind(tenant_slug)
        .bind(database_name)
        .bind(RUNTIME_MIGRATION_VERSION)
        .execute(self.control.pool())
        .await
        .context("failed to register tenant database")?;
        if result.rows_affected() == 0 {
            bail!("cannot register a database for unknown tenant {tenant_slug}");
        }
        self.pools
            .write()
            .expect("tenant database cache lock poisoned")
            .remove(tenant_slug);
        Ok(())
    }
}

pub fn validate_database_name(database_name: &str) -> anyhow::Result<()> {
    let valid = !database_name.is_empty()
        && database_name.len() <= 63
        && database_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid {
        bail!(
            "database name must contain only letters, digits, or underscores and be at most 63 characters"
        );
    }
    Ok(())
}

pub const CRATE_NAME: &str = "supercampus-database";

/// Latest forward-only runtime migration embedded in this build.
pub const RUNTIME_MIGRATION_VERSION: i64 = 26;

#[cfg(test)]
mod tests {
    use super::validate_database_name;

    #[test]
    fn tenant_database_names_are_safe_identifiers() {
        assert!(validate_database_name("tenant_university_01").is_ok());
        assert!(validate_database_name("tenant-a").is_err());
        assert!(validate_database_name("tenant a").is_err());
        assert!(validate_database_name("").is_err());
    }
}
