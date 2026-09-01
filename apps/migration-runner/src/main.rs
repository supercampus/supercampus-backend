#![forbid(unsafe_code)]

use std::{env, str::FromStr};

use anyhow::{Context, bail};
use futures_util::TryStreamExt;
use sqlx::{
    Row,
    postgres::{PgConnectOptions, PgPoolCopyExt, PgRow},
};
use supercampus_database::{Database, TenantDatabaseManager, validate_database_name};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("migration-runner");

    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str).unwrap_or("migrate") {
        "migrate" => migrate_registered_databases().await,
        "apply-mec-advisors" => apply_mec_advisors().await,
        "apply-mec-original-faculty" => apply_mec_original_faculty().await,
        "apply-mec-faculty-matrix" => apply_mec_faculty_matrix().await,
        "apply-student-assessments" => apply_student_assessments().await,
        "apply-push-notification-foundation" => apply_push_notification_foundation().await,
        "apply-parent-warden-gatepass-portals" => apply_parent_warden_gatepass_portals().await,
        "apply-stationery-inventory-pricing" => apply_stationery_inventory_pricing().await,
        "apply-leave-pass-approval-matrix" => apply_leave_pass_approval_matrix().await,
        "repair-mec-geofence" => repair_mec_geofence().await,
        "split-control-plane" => split_control_plane().await,
        "sync-control-plane" => sync_control_plane().await,
        "inspect-source" => inspect_source().await,
        "route-existing" => {
            let tenant_slug = args
                .get(1)
                .context("usage: migration-runner route-existing <tenant-slug> <database-name>")?;
            let database_name = args
                .get(2)
                .context("usage: migration-runner route-existing <tenant-slug> <database-name>")?;
            route_existing_tenant(tenant_slug, database_name).await
        }
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
            "unknown command {command}; expected migrate, apply-mec-advisors, apply-mec-original-faculty, apply-mec-faculty-matrix, apply-student-assessments, apply-push-notification-foundation, apply-parent-warden-gatepass-portals, repair-mec-geofence, inspect-source, split-control-plane, sync-control-plane, route-existing, or provision"
        ),
    }
}

/// Applies the role grants used by the advisor/HOD -> principal leave-pass chain.
async fn apply_leave_pass_approval_matrix() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0082_leave_pass_approval_matrix.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply leave-pass matrix to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id=registry.tenant_id
           WHERE registry.status='active' AND tenant.status='active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await?;
    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply leave-pass matrix to {slug}"))?;
    }
    println!(
        "applied leave-pass matrix to control and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Adds editable cost pricing before the API starts reading the new field.
/// The SQL is idempotent so this command is safe on every deployment restart.
async fn apply_stationery_inventory_pricing() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0081_stationery_inventory_item_pricing.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply stationery inventory pricing to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply stationery inventory pricing to {slug}"))?;
    }
    println!(
        "applied stationery inventory pricing to control plane and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Applies the isolated MEC parent/warden accounts and outpass schema without
/// forcing a legacy installation through the checksum-validated full chain.
async fn apply_parent_warden_gatepass_portals() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0078_parent_warden_gatepass_portals.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply parent/warden portals to the control plane")?;

    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    let mec = manager.tenant("mec").await?;
    sqlx::raw_sql(SQL)
        .execute(mec.pool())
        .await
        .context("failed to apply parent/warden portals to MEC")?;
    println!("applied MEC parent and warden gatepass portals");
    Ok(())
}

/// Applies only the idempotent push-notification foundation when a legacy
/// installation cannot use the checksum-validated full migration chain.
async fn apply_push_notification_foundation() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0077_push_notification_foundation.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply push notifications to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply push notifications to {slug}"))?;
    }

    println!(
        "applied push notification foundation to control and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Applies the isolated student-assessment table to the control plane and all
/// registered institution databases. This remains safe to repeat because the
/// migration contains only IF NOT EXISTS statements.
async fn apply_student_assessments() -> anyhow::Result<()> {
    const SQL: &str = include_str!("../../../migrations/runtime/0064_student_assessment_marks.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply student assessments to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply student assessments to {slug}"))?;
    }
    println!(
        "applied student assessments to control and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Applies the isolated MEC advisor change when a legacy installation cannot
/// run the full migrator because an old migration checksum predates immutable
/// migration enforcement. This deliberately does not edit migration history.
async fn apply_mec_advisors() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0063_class_advisor_assignments.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply advisor schema to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply advisor schema to {slug}"))?;
    }
    println!(
        "applied MEC advisor assignments to control and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Corrects the MEC timetable faculty matrix without running the full legacy
/// migration chain. The SQL is tenant-scoped and idempotent.
async fn apply_mec_faculty_matrix() -> anyhow::Result<()> {
    const SQL: &str =
        include_str!("../../../migrations/runtime/0068_mec_timetable_faculty_matrix.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    let mec = manager.tenant("mec").await?;
    sqlx::raw_sql(SQL)
        .execute(mec.pool())
        .await
        .context("failed to apply the MEC timetable faculty matrix")?;
    println!("applied the MEC timetable faculty matrix");
    Ok(())
}

/// Replaces the MEC placeholder faculty identities with the institution's
/// original faculty roster. The SQL is idempotent and preserves referenced
/// identity IDs so timetable, attendance, and audit relationships remain valid.
async fn apply_mec_original_faculty() -> anyhow::Result<()> {
    const SQL: &str = include_str!("../../../migrations/runtime/0067_mec_original_faculty.sql");
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    sqlx::raw_sql(SQL)
        .execute(control.pool())
        .await
        .context("failed to apply the MEC faculty roster to the control plane")?;

    let databases: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT tenant.slug, registry.database_name
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE registry.status = 'active' AND tenant.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list tenant databases")?;

    let base_options =
        PgConnectOptions::from_str(&control_url).context("invalid CONTROL_DATABASE_URL")?;
    for (slug, database_name) in &databases {
        validate_database_name(database_name)?;
        let tenant = Database::connect_options(base_options.clone().database(database_name), 2)
            .await
            .with_context(|| format!("failed to connect tenant {slug} database"))?;
        sqlx::raw_sql(SQL)
            .execute(tenant.pool())
            .await
            .with_context(|| format!("failed to apply the MEC faculty roster to {slug}"))?;
    }
    println!(
        "applied the MEC original faculty roster to control and {} tenant database(s)",
        databases.len()
    );
    Ok(())
}

/// Applies the guarded MEC coordinate correction even when legacy migration
/// checksums prevent the general release migrator from advancing. The WHERE
/// clause makes the repair idempotent and refuses to overwrite any later edit.
async fn repair_mec_geofence() -> anyhow::Result<()> {
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    let mec = manager.tenant("mec").await?;
    let result = sqlx::query(
        r#"UPDATE core.campuses AS campus
           SET metadata = jsonb_set(
               COALESCE(campus.metadata, '{}'::jsonb),
               '{geofence}',
               COALESCE(campus.metadata -> 'geofence', '{}'::jsonb)
                   || jsonb_build_object(
                       'latitude', 12.9277504,
                       'longitude', 79.9926235
                   ),
               true
           )
           FROM platform.tenants AS tenant
           WHERE campus.tenant_id = tenant.id
             AND tenant.slug = 'mec'
             AND campus.active
             AND abs((campus.metadata -> 'geofence' ->> 'latitude')::double precision
                     - 13.0104) < 0.000001
             AND abs((campus.metadata -> 'geofence' ->> 'longitude')::double precision
                     - 80.2356) < 0.000001"#,
    )
    .execute(mec.pool())
    .await
    .context("failed to repair the MEC campus geofence")?;

    println!(
        "MEC geofence repair complete; {} stale campus record(s) corrected",
        result.rows_affected()
    );
    Ok(())
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

async fn sync_control_plane() -> anyhow::Result<()> {
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
    let control = Database::connect(&control_url).await?;
    control.migrate().await?;

    let tenant = sqlx::query(
        r#"SELECT id, slug, code, name, city, status, created_at, updated_at
           FROM platform.tenants
           WHERE slug = $1 AND status = 'active'"#,
    )
    .bind(&primary_tenant_slug)
    .fetch_optional(source.pool())
    .await
    .context("failed to read source institution")?
    .with_context(|| format!("unknown active institution {primary_tenant_slug}"))?;

    let tenant_id = tenant.try_get::<uuid::Uuid, _>("id")?;
    let mut transaction = control.pool().begin().await?;
    sqlx::query("DELETE FROM platform.tenant_databases WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM authz.user_roles WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM authz.role_permissions WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM authz.roles WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM authz.permission_definitions WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM identity.tenant_memberships WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        r#"DELETE FROM identity.users
           WHERE id IN (SELECT user_id FROM identity.tenant_memberships WHERE tenant_id = $1)"#,
    )
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM platform.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;

    sqlx::query(
        r#"INSERT INTO platform.tenants
               (id, slug, code, name, city, status, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (slug) DO UPDATE SET
               id = EXCLUDED.id,
               code = EXCLUDED.code,
               name = EXCLUDED.name,
               city = EXCLUDED.city,
               status = EXCLUDED.status,
               updated_at = EXCLUDED.updated_at"#,
    )
    .bind(tenant_id)
    .bind(tenant.try_get::<String, _>("slug")?)
    .bind(tenant.try_get::<String, _>("code")?)
    .bind(tenant.try_get::<String, _>("name")?)
    .bind(tenant.try_get::<String, _>("city")?)
    .bind(tenant.try_get::<String, _>("status")?)
    .bind(tenant.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
    .bind(tenant.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
    .execute(&mut *transaction)
    .await?;

    let users = fetch_source_rows(
        &source,
        r#"SELECT id, email, password_hash, display_name, initials, account_type, active,
                  profile, created_at, updated_at, last_login_at
           FROM identity.users
           WHERE id IN (SELECT user_id FROM identity.tenant_memberships WHERE tenant_id = $1)"#,
        tenant_id,
    )
    .await?;
    for row in users {
        sqlx::query(
            r#"INSERT INTO identity.users
                   (id, email, password_hash, display_name, initials, account_type, active,
                    profile, created_at, updated_at, last_login_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
               ON CONFLICT (email) DO UPDATE SET
                   id = EXCLUDED.id,
                   password_hash = EXCLUDED.password_hash,
                   display_name = EXCLUDED.display_name,
                   initials = EXCLUDED.initials,
                   account_type = EXCLUDED.account_type,
                   active = EXCLUDED.active,
                   profile = EXCLUDED.profile,
                   updated_at = EXCLUDED.updated_at,
                   last_login_at = EXCLUDED.last_login_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("id")?)
        .bind(row.try_get::<String, _>("email")?)
        .bind(row.try_get::<String, _>("password_hash")?)
        .bind(row.try_get::<String, _>("display_name")?)
        .bind(row.try_get::<String, _>("initials")?)
        .bind(row.try_get::<String, _>("account_type")?)
        .bind(row.try_get::<bool, _>("active")?)
        .bind(row.try_get::<serde_json::Value, _>("profile")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
        .bind(row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_login_at")?)
        .execute(&mut *transaction)
        .await?;
    }

    let memberships = fetch_source_rows(
        &source,
        r#"SELECT tenant_id, user_id, roles, active, is_primary, profile, created_at, updated_at
           FROM identity.tenant_memberships
           WHERE tenant_id = $1"#,
        tenant_id,
    )
    .await?;
    for row in memberships {
        sqlx::query(
            r#"INSERT INTO identity.tenant_memberships
                   (tenant_id, user_id, roles, active, is_primary, profile, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (tenant_id, user_id) DO UPDATE SET
                   roles = EXCLUDED.roles,
                   active = EXCLUDED.active,
                   is_primary = EXCLUDED.is_primary,
                   profile = EXCLUDED.profile,
                   updated_at = EXCLUDED.updated_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("tenant_id")?)
        .bind(row.try_get::<uuid::Uuid, _>("user_id")?)
        .bind(row.try_get::<Vec<String>, _>("roles")?)
        .bind(row.try_get::<bool, _>("active")?)
        .bind(row.try_get::<bool, _>("is_primary")?)
        .bind(row.try_get::<serde_json::Value, _>("profile")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
        .execute(&mut *transaction)
        .await?;
    }

    sync_permission_definitions(&source, &mut transaction, tenant_id).await?;
    sync_roles(&source, &mut transaction, tenant_id).await?;
    sync_role_permissions(&source, &mut transaction, tenant_id).await?;
    sync_user_roles(&source, &mut transaction, tenant_id).await?;

    transaction.commit().await?;

    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    manager
        .register(&primary_tenant_slug, &source_database_name)
        .await?;
    println!(
        "control plane synced; institution {primary_tenant_slug} is routed to database {source_database_name}"
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

async fn route_existing_tenant(tenant_slug: &str, database_name: &str) -> anyhow::Result<()> {
    validate_database_name(database_name)?;
    let control_url = required_environment("CONTROL_DATABASE_URL")?;
    let control = Database::connect(&control_url).await?;
    control.migrate().await?;
    let manager = TenantDatabaseManager::clustered(control, &control_url)?;
    manager.register(tenant_slug, database_name).await?;
    println!("institution {tenant_slug} routed to existing database {database_name}");
    Ok(())
}

async fn fetch_source_rows(
    source: &Database,
    sql: &str,
    tenant_id: uuid::Uuid,
) -> anyhow::Result<Vec<PgRow>> {
    sqlx::query(sql)
        .bind(tenant_id)
        .fetch_all(source.pool())
        .await
        .context("failed to fetch source control-plane rows")
}

async fn sync_permission_definitions(
    source: &Database,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let rows = fetch_source_rows(
        source,
        r#"SELECT tenant_id, permission_key, module_key, feature_key, action, display_name,
                  description, active, created_at, updated_at, crud_actions
           FROM authz.permission_definitions
           WHERE tenant_id = $1"#,
        tenant_id,
    )
    .await?;
    for row in rows {
        sqlx::query(
            r#"INSERT INTO authz.permission_definitions
                   (tenant_id, permission_key, module_key, feature_key, action, display_name,
                    description, active, created_at, updated_at, crud_actions)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
               ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
                   module_key = EXCLUDED.module_key,
                   feature_key = EXCLUDED.feature_key,
                   action = EXCLUDED.action,
                   display_name = EXCLUDED.display_name,
                   description = EXCLUDED.description,
                   active = EXCLUDED.active,
                   crud_actions = EXCLUDED.crud_actions,
                   updated_at = EXCLUDED.updated_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("tenant_id")?)
        .bind(row.try_get::<String, _>("permission_key")?)
        .bind(row.try_get::<String, _>("module_key")?)
        .bind(row.try_get::<String, _>("feature_key")?)
        .bind(row.try_get::<String, _>("action")?)
        .bind(row.try_get::<String, _>("display_name")?)
        .bind(row.try_get::<String, _>("description")?)
        .bind(row.try_get::<bool, _>("active")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
        .bind(row.try_get::<Vec<String>, _>("crud_actions")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn sync_roles(
    source: &Database,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let rows = fetch_source_rows(
        source,
        r#"SELECT id, tenant_id, role_key, name, team, scope_description, portal_family, protected, active,
                  created_by, updated_by, created_at, updated_at
           FROM authz.roles
           WHERE tenant_id = $1"#,
        tenant_id,
    )
    .await?;
    for row in rows {
        sqlx::query(
            r#"INSERT INTO authz.roles
                   (id, tenant_id, role_key, name, team, scope_description, portal_family, protected, active,
                    created_by, updated_by, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
               ON CONFLICT (tenant_id, role_key) DO UPDATE SET
                   id = EXCLUDED.id,
                   name = EXCLUDED.name,
                   team = EXCLUDED.team,
                   scope_description = EXCLUDED.scope_description,
                   portal_family = EXCLUDED.portal_family,
                   protected = EXCLUDED.protected,
                   active = EXCLUDED.active,
                   updated_by = EXCLUDED.updated_by,
                   updated_at = EXCLUDED.updated_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("id")?)
        .bind(row.try_get::<uuid::Uuid, _>("tenant_id")?)
        .bind(row.try_get::<String, _>("role_key")?)
        .bind(row.try_get::<String, _>("name")?)
        .bind(row.try_get::<String, _>("team")?)
        .bind(row.try_get::<String, _>("scope_description")?)
        .bind(row.try_get::<String, _>("portal_family")?)
        .bind(row.try_get::<bool, _>("protected")?)
        .bind(row.try_get::<bool, _>("active")?)
        .bind(row.try_get::<String, _>("created_by")?)
        .bind(row.try_get::<String, _>("updated_by")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn sync_role_permissions(
    source: &Database,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let rows = fetch_source_rows(
        source,
        r#"SELECT tenant_id, role_id, surface, permission_key, scope, constraints, granted_by, granted_at
           FROM authz.role_permissions
           WHERE tenant_id = $1"#,
        tenant_id,
    )
    .await?;
    for row in rows {
        sqlx::query(
            r#"INSERT INTO authz.role_permissions
                   (tenant_id, role_id, surface, permission_key, scope, constraints, granted_by, granted_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
                   scope = EXCLUDED.scope,
                   constraints = EXCLUDED.constraints,
                   granted_by = EXCLUDED.granted_by,
                   granted_at = EXCLUDED.granted_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("tenant_id")?)
        .bind(row.try_get::<uuid::Uuid, _>("role_id")?)
        .bind(row.try_get::<String, _>("surface")?)
        .bind(row.try_get::<String, _>("permission_key")?)
        .bind(row.try_get::<String, _>("scope")?)
        .bind(row.try_get::<serde_json::Value, _>("constraints")?)
        .bind(row.try_get::<String, _>("granted_by")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("granted_at")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn sync_user_roles(
    source: &Database,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let rows = fetch_source_rows(
        source,
        r#"SELECT tenant_id, user_id, role_id, assigned_by, assigned_at
           FROM authz.user_roles
           WHERE tenant_id = $1"#,
        tenant_id,
    )
    .await?;
    for row in rows {
        sqlx::query(
            r#"INSERT INTO authz.user_roles
                   (tenant_id, user_id, role_id, assigned_by, assigned_at)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (tenant_id, user_id, role_id) DO UPDATE SET
                   assigned_by = EXCLUDED.assigned_by,
                   assigned_at = EXCLUDED.assigned_at"#,
        )
        .bind(row.try_get::<uuid::Uuid, _>("tenant_id")?)
        .bind(row.try_get::<uuid::Uuid, _>("user_id")?)
        .bind(row.try_get::<uuid::Uuid, _>("role_id")?)
        .bind(row.try_get::<String, _>("assigned_by")?)
        .bind(row.try_get::<chrono::DateTime<chrono::Utc>, _>("assigned_at")?)
        .execute(&mut **transaction)
        .await?;
    }
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
