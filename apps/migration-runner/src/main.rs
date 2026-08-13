#![forbid(unsafe_code)]

use std::{env, str::FromStr};

use anyhow::{Context, bail};
use futures_util::TryStreamExt;
use sqlx::{
    Row,
    postgres::{PgConnectOptions, PgPoolCopyExt},
};
use supercampus_database::{Database, TenantDatabaseManager, validate_database_name};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("migration-runner");

    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str).unwrap_or("migrate") {
        "migrate" => migrate_registered_databases().await,
        "split-control-plane" => split_control_plane().await,
        "inspect-source" => inspect_source().await,
        "provision" => {
            let tenant_slug = args
                .get(1)
                .context("usage: migration-runner provision <tenant-slug> <database-name>")?;
            let database_name = args
                .get(2)
                .context("usage: migration-runner provision <tenant-slug> <database-name>")?;
            provision_tenant(tenant_slug, database_name).await
        }
        command => bail!(
            "unknown command {command}; expected migrate, inspect-source, split-control-plane, or provision"
        ),
    }
}

async fn inspect_source() -> anyhow::Result<()> {
    let source_url = required_environment("DATABASE_URL")?;
    let source = Database::connect(&source_url).await?;
    source.migrate().await?;
    let rows = sqlx::query(
        r#"SELECT tenant.slug, tenant.code, tenant.name, tenant.status,
                  (SELECT count(*) FROM identity.tenant_memberships membership
                   WHERE membership.tenant_id = tenant.id) AS memberships,
                  (SELECT count(*) FROM authz.roles role
                   WHERE role.tenant_id = tenant.id) AS roles,
                  (SELECT count(*) FROM crm.leads lead
                   WHERE lead.tenant_id = tenant.id) AS leads,
                  (SELECT count(*) FROM crm.forms form
                   WHERE form.tenant_id = tenant.id) AS forms
           FROM platform.tenants tenant
           ORDER BY tenant.created_at, tenant.slug"#,
    )
    .fetch_all(source.pool())
    .await
    .context("failed to inspect source institutions")?;

    println!("slug | code | name | status | memberships | roles | leads | forms");
    for row in rows {
        println!(
            "{} | {} | {} | {} | {} | {} | {} | {}",
            row.try_get::<String, _>("slug")?,
            row.try_get::<String, _>("code")?,
            row.try_get::<String, _>("name")?,
            row.try_get::<String, _>("status")?,
            row.try_get::<i64, _>("memberships")?,
            row.try_get::<i64, _>("roles")?,
            row.try_get::<i64, _>("leads")?,
            row.try_get::<i64, _>("forms")?,
        );
    }
    Ok(())
}
async fn migrate_registered_databases() -> anyhow::Result<()> {
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    control.migrate().await?;
    let manager = TenantDatabaseManager::clustered(control.clone(), &control_url)?;

    let slugs: Vec<String> = sqlx::query_scalar(
        r#"SELECT tenant.slug
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    for slug in &slugs {
        manager.tenant(slug).await?;
    }
    println!(
        "migrated control plane and {} registered institution database(s)",
        slugs.len()
    );
    Ok(())
}

async fn split_control_plane() -> anyhow::Result<()> {
    let source_url = required_environment("DATABASE_URL")?;
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let primary_tenant_slug =
        env::var("PRIMARY_TENANT_SLUG").unwrap_or_else(|_| "tenant-local".to_owned());

    let source_options = PgConnectOptions::from_str(&source_url).context("invalid DATABASE_URL")?;
    let source_database_name = source_options
        .get_database()
        .context("DATABASE_URL must name the existing institution database")?
        .to_owned();
    validate_database_name(&source_database_name)?;
    ensure_database_exists(&control_url).await?;

    let source = Database::connect(&source_url).await?;
    source.migrate().await?;
    let primary_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM platform.tenants WHERE slug = $1 AND status = 'active')",
    )
    .bind(&primary_tenant_slug)
    .fetch_one(source.pool())
    .await
    .context("failed to validate PRIMARY_TENANT_SLUG")?;
    if !primary_exists {
        bail!("PRIMARY_TENANT_SLUG {primary_tenant_slug} is not active in the source database");
    }

    let control = Database::connect(&control_url).await?;
    control.migrate().await?;
    sqlx::query(
        r#"TRUNCATE TABLE
               identity.auth_sessions,
               authz.user_roles,
               authz.role_permissions,
               authz.roles,
               authz.permission_definitions,
               identity.tenant_memberships,
               identity.users,
               platform.tenants
           CASCADE"#,
    )
    .execute(control.pool())
    .await
    .context("failed to prepare the control plane for identity migration")?;

    copy_control_plane_tenant(&source, &control, &primary_tenant_slug).await?;

    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    manager
        .register(&primary_tenant_slug, &source_database_name)
        .await?;
    manager.tenant(&primary_tenant_slug).await?;

    println!(
        "control plane initialized; institution {primary_tenant_slug} is routed to database {source_database_name}"
    );
    Ok(())
}

async fn provision_tenant(tenant_slug: &str, database_name: &str) -> anyhow::Result<()> {
    validate_database_name(database_name)?;
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    ensure_named_database_exists(&control_url, database_name).await?;

    let control = Database::connect(&control_url).await?;
    control.migrate().await?;
    let control_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    let tenant = Database::connect_options(control_options.database(database_name), 5).await?;
    tenant.migrate().await?;

    let row = sqlx::query(
        r#"SELECT id, slug, code, name, city, status, created_at, updated_at
           FROM platform.tenants
           WHERE slug = $1 AND status = 'active'"#,
    )
    .bind(tenant_slug)
    .fetch_optional(control.pool())
    .await
    .context("failed to read institution from the control plane")?
    .with_context(|| format!("unknown active institution {tenant_slug}"))?;

    let mut transaction = tenant.pool().begin().await?;
    sqlx::query("DELETE FROM platform.tenants")
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        r#"INSERT INTO platform.tenants
               (id, slug, code, name, city, status, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(row.try_get::<uuid::Uuid, _>("id")?)
    .bind(row.try_get::<String, _>("slug")?)
    .bind(row.try_get::<String, _>("code")?)
    .bind(row.try_get::<String, _>("name")?)
    .bind(row.try_get::<String, _>("city")?)
    .bind(row.try_get::<String, _>("status")?)
    .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
    .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    manager.register(tenant_slug, database_name).await?;
    manager.tenant(tenant_slug).await?;
    println!("institution {tenant_slug} provisioned in database {database_name}");
    Ok(())
}

async fn ensure_database_exists(database_url: &str) -> anyhow::Result<()> {
    let options =
        PgConnectOptions::from_str(database_url).context("invalid CONTROL_DATABASE_URL")?;
    let database_name = options
        .get_database()
        .context("CONTROL_DATABASE_URL must include a database name")?
        .to_owned();
    ensure_named_database_exists(database_url, &database_name).await
}

async fn ensure_named_database_exists(server_url: &str, database_name: &str) -> anyhow::Result<()> {
    validate_database_name(database_name)?;
    let options = PgConnectOptions::from_str(server_url)
        .context("invalid PostgreSQL server URL")?
        .database("postgres");
    let server = Database::connect_options(options, 1).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(database_name)
            .fetch_one(server.pool())
            .await
            .context("failed to inspect PostgreSQL databases")?;
    if !exists {
        let statement = format!(r#"CREATE DATABASE "{database_name}""#);
        sqlx::query(&statement)
            .execute(server.pool())
            .await
            .with_context(|| format!("failed to create database {database_name}"))?;
    }
    Ok(())
}

async fn copy_control_plane_tenant(
    source: &Database,
    target: &Database,
    tenant_slug: &str,
) -> anyhow::Result<()> {
    let slug = tenant_slug.replace('\'', "''");
    let tenant_id = format!("(SELECT id FROM platform.tenants WHERE slug = '{slug}')");
    copy_query(
        source,
        target,
        "platform.tenants",
        &format!("SELECT * FROM platform.tenants WHERE slug = '{slug}'"),
    )
    .await?;
    sqlx::query(
        r#"TRUNCATE TABLE
               authz.user_roles,
               authz.role_permissions,
               authz.roles,
               authz.permission_definitions
           CASCADE"#,
    )
    .execute(target.pool())
    .await
    .context("failed to clear bootstrap authorization rows")?;

    let copies = [
        (
            "identity.users",
            format!(
                "SELECT * FROM identity.users WHERE id IN \
                 (SELECT user_id FROM identity.tenant_memberships WHERE tenant_id = {tenant_id})"
            ),
        ),
        (
            "identity.tenant_memberships",
            format!("SELECT * FROM identity.tenant_memberships WHERE tenant_id = {tenant_id}"),
        ),
        (
            "authz.permission_definitions",
            format!("SELECT * FROM authz.permission_definitions WHERE tenant_id = {tenant_id}"),
        ),
        (
            "authz.roles",
            format!("SELECT * FROM authz.roles WHERE tenant_id = {tenant_id}"),
        ),
        (
            "authz.role_permissions",
            format!("SELECT * FROM authz.role_permissions WHERE tenant_id = {tenant_id}"),
        ),
        (
            "authz.user_roles",
            format!("SELECT * FROM authz.user_roles WHERE tenant_id = {tenant_id}"),
        ),
        (
            "identity.auth_sessions",
            format!("SELECT * FROM identity.auth_sessions WHERE tenant_id = {tenant_id}"),
        ),
    ];

    for (table, select) in copies {
        copy_query(source, target, table, &select).await?;
    }
    Ok(())
}

async fn copy_query(
    source: &Database,
    target: &Database,
    table: &str,
    select: &str,
) -> anyhow::Result<()> {
    let export = format!("COPY ({select}) TO STDOUT WITH (FORMAT BINARY)");
    let import = format!("COPY {table} FROM STDIN WITH (FORMAT BINARY)");
    let mut stream = source
        .pool()
        .copy_out_raw(&export)
        .await
        .with_context(|| format!("failed to export {table}"))?;
    let mut sink = target
        .pool()
        .copy_in_raw(&import)
        .await
        .with_context(|| format!("failed to import {table}"))?;
    while let Some(bytes) = stream
        .try_next()
        .await
        .with_context(|| format!("failed while reading {table}"))?
    {
        sink.send(bytes)
            .await
            .with_context(|| format!("failed while writing {table}"))?;
    }
    sink.finish()
        .await
        .with_context(|| format!("failed to finish importing {table}"))?;
    Ok(())
}

fn required_environment(name: &str) -> anyhow::Result<String> {
    env::var(name).with_context(|| format!("{name} is required"))
}
