//! Minimum and latest supported app versions, per tenant and platform.
//!
//! The policy lives in the control plane (`platform.app_version_policies`),
//! one row per tenant and platform, managed from the tenant's Admin Desk.
//! A single store binary serves every tenant, so the public read used by the
//! app's startup check takes the signed-in tenant when the app knows it and
//! falls back to the deployment's primary tenant (`PRIMARY_TENANT_SLUG`,
//! then the most recently updated policy) before sign-in.

use std::{
    cmp::Ordering,
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub const READ_PERMISSION: &str = "administration.app_versions.read";
pub const UPDATE_PERMISSION: &str = "administration.app_versions.update";
pub const PLATFORMS: [&str; 2] = ["android", "ios"];

/// Authenticated routes, merged into `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/app-versions", get(admin_list))
        .route(
            "/admin/app-versions/{platform}",
            get(admin_get).put(admin_update),
        )
}

/// Applies migrations/runtime/0131 once per process.
async fn ensure_schema(pool: &PgPool) {
    static READY: AtomicBool = AtomicBool::new(false);
    if READY.load(AtomicOrdering::Acquire) {
        return;
    }
    match sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0131_app_version_policies.sql"
    ))
    .execute(pool)
    .await
    {
        Ok(_) => READY.store(true, AtomicOrdering::Release),
        Err(error) => tracing::warn!(%error, "app version schema not applied"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppVersionPolicy {
    pub platform: String,
    pub latest_version: String,
    pub minimum_version: String,
    pub store_url: String,
    pub force_update: bool,
    pub updated_by: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl AppVersionPolicy {
    /// What an unconfigured platform answers: nothing is required.
    fn unconfigured(platform: &str) -> Self {
        Self {
            platform: platform.to_owned(),
            latest_version: "0.0.0".into(),
            minimum_version: "0.0.0".into(),
            store_url: String::new(),
            force_update: false,
            updated_by: None,
            updated_at: None,
        }
    }
}

/// Parses `1.2.3`, `1.2`, `1.2.3+45` or `1.2.3-beta` into numeric parts.
/// Build metadata and pre-release suffixes are ignored.
pub fn parse_version(value: &str) -> Option<Vec<u64>> {
    let core = value
        .trim()
        .split(['+', '-'])
        .next()
        .unwrap_or_default()
        .trim_start_matches(['v', 'V']);
    if core.is_empty() {
        return None;
    }
    let parts = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (1..=4).contains(&parts.len()).then_some(parts)
}

pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let mut left = parse_version(left)?;
    let mut right = parse_version(right)?;
    let length = left.len().max(right.len());
    left.resize(length, 0);
    right.resize(length, 0);
    Some(left.cmp(&right))
}

/// `required` blocks the app, `recommended` shows a dismissible prompt.
pub fn update_status(current: &str, policy: &AppVersionPolicy) -> &'static str {
    let below = |target: &str| compare_versions(current, target) == Some(Ordering::Less);
    if below(&policy.minimum_version) || (policy.force_update && below(&policy.latest_version)) {
        "required"
    } else if below(&policy.latest_version) {
        "recommended"
    } else {
        "current"
    }
}

fn normalize_platform(value: &str) -> ApiResult<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "android" => Ok("android"),
        "ios" | "iphone" | "ipad" => Ok("ios"),
        _ => Err(ApiError::BadRequest(
            "Platform must be android or ios".into(),
        )),
    }
}

fn row_to_policy(row: &sqlx::postgres::PgRow) -> Result<AppVersionPolicy, sqlx::Error> {
    Ok(AppVersionPolicy {
        platform: row.try_get("platform")?,
        latest_version: row.try_get("latest_version")?,
        minimum_version: row.try_get("minimum_version")?,
        store_url: row.try_get("store_url")?,
        force_update: row.try_get("force_update")?,
        updated_by: row.try_get("updated_by")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn tenant_policies(pool: &PgPool, tenant_slug: &str) -> ApiResult<Vec<AppVersionPolicy>> {
    let rows = sqlx::query(
        r#"SELECT policy.platform, policy.latest_version, policy.minimum_version,
                  policy.store_url, policy.force_update, policy.updated_by, policy.updated_at
           FROM platform.app_version_policies policy
           JOIN platform.tenants tenant ON tenant.id = policy.tenant_id
           WHERE tenant.slug = $1"#,
    )
    .bind(tenant_slug)
    .fetch_all(pool)
    .await?;
    let stored = rows
        .iter()
        .map(row_to_policy)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PLATFORMS
        .iter()
        .map(|platform| {
            stored
                .iter()
                .find(|policy| policy.platform == *platform)
                .cloned()
                .unwrap_or_else(|| AppVersionPolicy::unconfigured(platform))
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicQuery {
    platform: Option<String>,
    tenant: Option<String>,
    /// The caller's version; when given, the answer includes `status`.
    current: Option<String>,
}

/// `GET /api/app-version` — unauthenticated, read by the app at startup.
pub async fn public_app_version(
    State(state): State<AppState>,
    Query(query): Query<PublicQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let platform = normalize_platform(query.platform.as_deref().unwrap_or("android"))?;
    let policy = match state.database() {
        Some(database) => {
            let pool = database.pool();
            ensure_schema(pool).await;
            let tenant = query
                .tenant
                .as_deref()
                .map(str::trim)
                .filter(|slug| !slug.is_empty() && slug.len() <= 64)
                .map(str::to_owned)
                .or_else(|| {
                    std::env::var("PRIMARY_TENANT_SLUG")
                        .ok()
                        .map(|slug| slug.trim().to_owned())
                        .filter(|slug| !slug.is_empty())
                });
            let row = sqlx::query(
                r#"SELECT policy.platform, policy.latest_version, policy.minimum_version,
                          policy.store_url, policy.force_update, policy.updated_by,
                          policy.updated_at
                   FROM platform.app_version_policies policy
                   JOIN platform.tenants tenant ON tenant.id = policy.tenant_id
                   WHERE policy.platform = $1
                     AND tenant.slug <> 'supercampus-control'
                   ORDER BY (tenant.slug = $2) DESC, policy.updated_at DESC
                   LIMIT 1"#,
            )
            .bind(platform)
            .bind(tenant.as_deref().unwrap_or(""))
            .fetch_optional(pool)
            .await?;
            match row {
                Some(row) => row_to_policy(&row)?,
                None => AppVersionPolicy::unconfigured(platform),
            }
        }
        None => AppVersionPolicy::unconfigured(platform),
    };
    let mut body = public_body(&policy);
    if let Some(current) = query
        .current
        .as_deref()
        .filter(|value| parse_version(value).is_some())
    {
        body["status"] = json!(update_status(current, &policy));
    }
    Ok(Json(ApiResponse::new(body)))
}

/// The public answer leaves out who changed the policy.
fn public_body(policy: &AppVersionPolicy) -> Value {
    json!({
        "platform": policy.platform,
        "latestVersion": policy.latest_version,
        "minimumVersion": policy.minimum_version,
        "storeUrl": policy.store_url,
        "forceUpdate": policy.force_update,
        "updatedAt": policy.updated_at,
    })
}

fn require_read(access: &EffectiveAccess) -> ApiResult<()> {
    if access.allows(READ_PERMISSION) || access.allows(UPDATE_PERMISSION) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn control_pool(state: &AppState) -> ApiResult<PgPool> {
    state
        .database()
        .map(|database| database.pool().clone())
        .ok_or_else(|| ApiError::ServiceUnavailable("App versions need the database".into()))
}

async fn admin_list(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_read(&access)?;
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let policies = tenant_policies(&pool, &principal.student.tenant_id).await?;
    Ok(Json(ApiResponse::new(json!({
        "policies": policies,
        "canUpdate": access.allows(UPDATE_PERMISSION),
    }))))
}

async fn admin_get(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(platform): Path<String>,
) -> ApiResult<Json<ApiResponse<AppVersionPolicy>>> {
    require_read(&access)?;
    let platform = normalize_platform(&platform)?;
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let policy = tenant_policies(&pool, &principal.student.tenant_id)
        .await?
        .into_iter()
        .find(|policy| policy.platform == platform)
        .unwrap_or_else(|| AppVersionPolicy::unconfigured(platform));
    Ok(Json(ApiResponse::new(policy)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePolicyRequest {
    latest_version: String,
    minimum_version: String,
    #[serde(default)]
    store_url: String,
    #[serde(default)]
    force_update: bool,
}

/// Validated, trimmed values ready to store.
pub fn validate_update(request: &UpdatePolicyRequest) -> Result<(String, String, String), String> {
    let latest = request.latest_version.trim();
    let minimum = request.minimum_version.trim();
    if parse_version(latest).is_none() {
        return Err("Latest version must look like 1.2.3".into());
    }
    if parse_version(minimum).is_none() {
        return Err("Minimum version must look like 1.2.3".into());
    }
    if compare_versions(minimum, latest) == Some(Ordering::Greater) {
        return Err("Minimum version cannot be above the latest version".into());
    }
    let store_url = request.store_url.trim();
    if !store_url.is_empty() {
        let valid = store_url.len() <= 500
            && store_url.starts_with("https://")
            && !store_url.chars().any(char::is_whitespace)
            && store_url.len() > "https://".len();
        if !valid {
            return Err("Store URL must be an https:// link".into());
        }
    }
    Ok((latest.to_owned(), minimum.to_owned(), store_url.to_owned()))
}

async fn admin_update(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(platform): Path<String>,
    Json(request): Json<UpdatePolicyRequest>,
) -> ApiResult<Json<ApiResponse<AppVersionPolicy>>> {
    if !access.allows(UPDATE_PERMISSION) {
        return Err(ApiError::Forbidden);
    }
    let platform = normalize_platform(&platform)?;
    let (latest, minimum, store_url) = validate_update(&request).map_err(ApiError::BadRequest)?;
    // A forced update with nowhere to go would strand every user behind it.
    if request.force_update && store_url.is_empty() {
        return Err(ApiError::BadRequest(
            "Add the store URL before requiring an update, so blocked users can reach it".into(),
        ));
    }
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let row = sqlx::query(
        r#"INSERT INTO platform.app_version_policies
               (tenant_id, platform, latest_version, minimum_version, store_url, force_update,
                updated_by, updated_at)
           SELECT tenant.id, $2, $3, $4, $5, $6, $7, now()
           FROM platform.tenants tenant WHERE tenant.slug = $1
           ON CONFLICT (tenant_id, platform) DO UPDATE SET
               latest_version = EXCLUDED.latest_version,
               minimum_version = EXCLUDED.minimum_version,
               store_url = EXCLUDED.store_url,
               force_update = EXCLUDED.force_update,
               updated_by = EXCLUDED.updated_by,
               updated_at = now()
           RETURNING platform, latest_version, minimum_version, store_url, force_update,
                     updated_by, updated_at"#,
    )
    .bind(&principal.student.tenant_id)
    .bind(platform)
    .bind(&latest)
    .bind(&minimum)
    .bind(&store_url)
    .bind(request.force_update)
    .bind(&principal.student.email)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Tenant not found".into()))?;
    Ok(Json(ApiResponse::new(row_to_policy(&row)?)))
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{
        AppVersionPolicy, UpdatePolicyRequest, compare_versions, parse_version, update_status,
        validate_update,
    };

    fn policy(latest: &str, minimum: &str, force: bool) -> AppVersionPolicy {
        AppVersionPolicy {
            platform: "android".into(),
            latest_version: latest.into(),
            minimum_version: minimum.into(),
            store_url: "https://play.google.com/store/apps/details?id=ai.supercampus.mobile".into(),
            force_update: force,
            updated_by: None,
            updated_at: None,
        }
    }

    #[test]
    fn versions_compare_numerically_and_ignore_build_metadata() {
        assert_eq!(parse_version("1.0.9+34"), Some(vec![1, 0, 9]));
        assert_eq!(parse_version("v2.1"), Some(vec![2, 1]));
        assert_eq!(parse_version("1.x"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(compare_versions("1.0.10", "1.0.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.1", "1.1.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.0.9", "1.2.0"), Some(Ordering::Less));
    }

    #[test]
    fn status_follows_minimum_latest_and_force() {
        assert_eq!(
            update_status("1.0.9", &policy("1.0.9", "1.0.0", false)),
            "current"
        );
        assert_eq!(
            update_status("1.0.8", &policy("1.0.9", "1.0.0", false)),
            "recommended"
        );
        assert_eq!(
            update_status("1.0.8", &policy("1.0.9", "1.0.0", true)),
            "required"
        );
        assert_eq!(
            update_status("0.9.0", &policy("1.0.9", "1.0.0", false)),
            "required"
        );
        assert_eq!(
            update_status("1.1.0", &policy("1.0.9", "1.0.9", true)),
            "current"
        );
    }

    #[test]
    fn updates_are_validated() {
        let request = |latest: &str, minimum: &str, url: &str| UpdatePolicyRequest {
            latest_version: latest.into(),
            minimum_version: minimum.into(),
            store_url: url.into(),
            force_update: false,
        };
        assert!(validate_update(&request("1.0.9", "1.0.0", "https://example.com/app")).is_ok());
        assert!(validate_update(&request("1.0.9", "1.1.0", "")).is_err());
        assert!(validate_update(&request("latest", "1.0.0", "")).is_err());
        assert!(validate_update(&request("1.0.9", "1.0.0", "http://example.com")).is_err());
        assert_eq!(
            validate_update(&request(" 1.0.9 ", " 1.0.0", " ")).unwrap(),
            ("1.0.9".into(), "1.0.0".into(), String::new())
        );
    }
}
