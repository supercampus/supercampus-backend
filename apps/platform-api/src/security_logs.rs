//! Security logs for the tenant Admin Desk: login sessions and sign-in events.
//!
//! Sessions come straight from `identity.auth_sessions` (one row per login,
//! single-device). `identity.auth_login_events` adds what sessions cannot
//! show: failed and refused sign-ins, sign-outs, administrator revocations,
//! and the client's platform, app version and IP. Events are written
//! best-effort in the background and never change how authentication behaves.

use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::require,
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub const READ_PERMISSION: &str = "administration.security_logs.read";
pub const REVOKE_PERMISSION: &str = "administration.security_logs.revoke";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/security-logs/sessions", get(list_sessions))
        .route("/admin/security-logs/events", get(list_events))
        .route(
            "/admin/security-logs/sessions/{session_id}/revoke",
            post(revoke_session),
        )
}

/// What the log queries read, applied on its own so nothing else in 0130 can
/// hold it back: the events table and the session device columns from 0076,
/// which databases stuck behind the duplicate 0076 never received (sessions
/// there keep the device only in `profile`).
const LOG_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS identity.auth_login_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text,
    email text,
    outcome text NOT NULL
        CHECK (outcome IN ('success', 'failure', 'blocked', 'signed_out', 'revoked')),
    reason text,
    session_id uuid,
    device_id text,
    device_name text,
    platform text,
    app_version text,
    ip_address text,
    user_agent text,
    actor_user_id text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS auth_login_events_tenant_created_idx
    ON identity.auth_login_events (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS auth_login_events_session_idx
    ON identity.auth_login_events (session_id)
    WHERE session_id IS NOT NULL;
ALTER TABLE identity.auth_sessions
    ADD COLUMN IF NOT EXISTS device_id text,
    ADD COLUMN IF NOT EXISTS device_name text;
"#;

/// Applies migrations/runtime/0130 once per process, for databases the
/// migrator cannot reach. The tables the logs read go first and on their own;
/// the permission seeding follows, and a failure there is only logged. Either
/// failure is retried on the next call.
pub(crate) async fn ensure_schema(pool: &PgPool) {
    static READY: AtomicBool = AtomicBool::new(false);
    if READY.load(Ordering::Acquire) {
        return;
    }
    if let Err(error) = sqlx::raw_sql(LOG_TABLES_SQL).execute(pool).await {
        tracing::warn!(%error, "security log tables not applied");
        return;
    }
    match sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0130_security_logs_and_audit_permissions.sql"
    ))
    .execute(pool)
    .await
    {
        Ok(_) => READY.store(true, Ordering::Release),
        Err(error) => tracing::warn!(%error, "security log permissions not applied"),
    }
}

/// What the request says about the client. Only headers are used: the API
/// sits behind a proxy, so the socket address is the proxy's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClientContext {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub platform: Option<String>,
    pub app_version: Option<String>,
}

pub(crate) fn client_context(headers: &HeaderMap, device_name: Option<&str>) -> ClientContext {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let ip_address = header("x-forwarded-for")
        .and_then(|value| value.split(',').next())
        .or_else(|| header("x-real-ip"))
        .or_else(|| header("cf-connecting-ip"))
        .map(str::trim)
        .filter(|value| value.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_owned);
    let user_agent = header("user-agent").map(|value| value.chars().take(300).collect::<String>());
    let platform = header("x-client-platform")
        .and_then(sanitize_platform)
        .or_else(|| device_name.and_then(platform_from_device_name))
        .or_else(|| user_agent.as_deref().and_then(platform_from_user_agent));
    let app_version = header("x-app-version").and_then(sanitize_version);
    ClientContext {
        ip_address,
        user_agent,
        platform,
        app_version,
    }
}

fn sanitize_platform(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()
        && value.len() <= 20
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(value)
}

fn sanitize_version(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 32
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '+' | '-')
        }))
    .then(|| value.to_owned())
}

/// The app names its device by platform ("Android device", "iPhone or iPad").
fn platform_from_device_name(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    let platform = if name.contains("android") {
        "android"
    } else if name.contains("iphone") || name.contains("ipad") {
        "ios"
    } else if name.contains("web") || name.contains("browser") {
        "web"
    } else if name.contains("windows") {
        "windows"
    } else if name.contains("mac") {
        "macos"
    } else if name.contains("linux") {
        "linux"
    } else {
        return None;
    };
    Some(platform.to_owned())
}

fn platform_from_user_agent(agent: &str) -> Option<String> {
    let platform = if agent.contains("Android") {
        "android"
    } else if agent.contains("iPhone") || agent.contains("iPad") {
        "ios"
    } else if agent.contains("Mozilla/") {
        "web"
    } else {
        return None;
    };
    Some(platform.to_owned())
}

/// One sign-in, refusal, sign-out or revocation to record.
#[derive(Clone, Debug, Default)]
pub(crate) struct LoginEvent {
    pub outcome: &'static str,
    pub reason: Option<String>,
    /// Tenant slug when known. Without it the tenant is resolved from `email`.
    pub tenant_slug: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub session_id: Option<Uuid>,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub actor_user_id: Option<String>,
}

/// Records the event in the background. Never fails the caller and never
/// delays the response; with no database (tests, memory mode) it does nothing.
pub(crate) fn record_in_background(state: &AppState, event: LoginEvent, client: ClientContext) {
    let Some(database) = state.database() else {
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = record(database.pool(), &event, &client).await {
            tracing::warn!(%error, outcome = event.outcome, "security log event not recorded");
        }
    });
}

async fn record(pool: &PgPool, event: &LoginEvent, client: &ClientContext) -> anyhow::Result<()> {
    ensure_schema(pool).await;
    let email = event
        .email
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let (tenant_id, user_id) = if let Some(slug) = event.tenant_slug.as_deref() {
        let tenant = sqlx::query_scalar::<_, Uuid>("SELECT id FROM platform.tenants WHERE slug=$1")
            .bind(slug)
            .fetch_optional(pool)
            .await?;
        (tenant, event.user_id.clone())
    } else if let Some(email) = email.as_deref() {
        // A failed attempt carries only the typed email. Attribute it to that
        // account's primary tenant; an email with no account is not stored.
        let row = sqlx::query(
            r#"SELECT u.id::text AS user_id, m.tenant_id
               FROM identity.users u
               JOIN identity.tenant_memberships m ON m.user_id = u.id
               WHERE u.email = $1
               ORDER BY m.is_primary DESC, m.active DESC
               LIMIT 1"#,
        )
        .bind(email)
        .fetch_optional(pool)
        .await?;
        match row {
            Some(row) => (
                Some(row.try_get::<Uuid, _>("tenant_id")?),
                Some(row.try_get::<String, _>("user_id")?),
            ),
            None => return Ok(()),
        }
    } else {
        (None, event.user_id.clone())
    };
    let Some(tenant_id) = tenant_id else {
        return Ok(());
    };
    sqlx::query(
        r#"INSERT INTO identity.auth_login_events
           (tenant_id, user_id, email, outcome, reason, session_id, device_id, device_name,
            platform, app_version, ip_address, user_agent, actor_user_id)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(email)
    .bind(event.outcome)
    .bind(event.reason.as_deref())
    .bind(event.session_id)
    .bind(truncate(event.device_id.as_deref(), 200))
    .bind(truncate(event.device_name.as_deref(), 120))
    .bind(client.platform.as_deref())
    .bind(client.app_version.as_deref())
    .bind(client.ip_address.as_deref())
    .bind(client.user_agent.as_deref())
    .bind(event.actor_user_id.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

fn truncate(value: Option<&str>, limit: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(limit).collect())
}

fn control_pool(state: &AppState) -> ApiResult<PgPool> {
    state
        .database()
        .map(|database| database.pool().clone())
        .ok_or_else(|| ApiError::ServiceUnavailable("Security logs need the database".into()))
}

async fn tenant_uuid(pool: &PgPool, slug: &str) -> ApiResult<Uuid> {
    sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound("Tenant not found".into()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogQuery {
    status: Option<String>,
    outcome: Option<String>,
    q: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Inclusive start and exclusive end instants; None leaves that side open.
pub(crate) type DayBounds = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// `from`/`to` are whole days in India time (the campus's day), `to` inclusive.
pub(crate) fn day_bounds(from: Option<NaiveDate>, to: Option<NaiveDate>) -> ApiResult<DayBounds> {
    if let (Some(from), Some(to)) = (from, to)
        && to < from
    {
        return Err(ApiError::BadRequest(
            "The end date must be on or after the start date".into(),
        ));
    }
    let ist = chrono::FixedOffset::east_opt(5 * 3600 + 1800).expect("valid offset");
    let start_of = |day: NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|time| time.and_local_timezone(ist).single())
            .map(|time| time.with_timezone(&Utc))
    };
    Ok((
        from.and_then(start_of),
        to.and_then(|day| day.succ_opt()).and_then(start_of),
    ))
}

/// `%term%` for ILIKE with the wildcard characters escaped, or None.
pub(crate) fn like_pattern(value: Option<&str>) -> Option<String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty())?;
    let escaped: String = value
        .chars()
        .take(100)
        .flat_map(|character| match character {
            '%' | '_' | '\\' => vec!['\\', character],
            other => vec![other],
        })
        .collect();
    Some(format!("%{escaped}%"))
}

/// Why a session ended, as shown to the administrator.
fn end_reason(
    status: &str,
    revoked_reason: Option<&str>,
    ended_outcome: Option<&str>,
) -> Option<&'static str> {
    match status {
        "active" => None,
        "expired" => Some("expired"),
        _ => Some(match (ended_outcome, revoked_reason) {
            (Some("revoked"), _) | (_, Some("admin_revoked")) => "revoked_by_admin",
            (Some("signed_out"), _) => "signed_out",
            (_, Some("signed_in_elsewhere")) => "signed_in_elsewhere",
            (_, Some("replaced")) => "signed_in_again",
            _ => "ended",
        }),
    }
}

async fn list_sessions(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, READ_PERMISSION)?;
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let tenant = tenant_uuid(&pool, &principal.student.tenant_id).await?;
    let (from, to) = day_bounds(query.from, query.to)?;
    let status = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| matches!(*value, "active" | "revoked" | "expired"));
    let pattern = like_pattern(query.q.as_deref());
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).clamp(0, 100_000);
    let row = sqlx::query(
        r#"WITH sessions AS (
             SELECT s.id, s.user_id, s.roles, s.created_at, s.last_seen_at, s.rotated_at,
                    s.expires_at, s.revoked_at,
                    COALESCE(NULLIF(u.display_name, ''), s.profile->>'name', '') AS name,
                    COALESCE(u.email, s.profile->>'email', '') AS email,
                    COALESCE(s.device_id, s.profile->>'_sessionDeviceId') AS device_id,
                    COALESCE(s.device_name, s.profile->>'_sessionDeviceName') AS device_name,
                    s.profile->>'_sessionRevokedReason' AS revoked_reason,
                    CASE WHEN s.revoked_at IS NOT NULL THEN 'revoked'
                         WHEN s.expires_at <= now() THEN 'expired'
                         ELSE 'active' END AS status
             FROM identity.auth_sessions s
             LEFT JOIN identity.users u ON u.id::text = s.user_id
             WHERE s.tenant_id = $1
               AND ($2::timestamptz IS NULL OR s.created_at >= $2)
               AND ($3::timestamptz IS NULL OR s.created_at < $3)
               AND ($4::text IS NULL
                    OR COALESCE(u.display_name, '') ILIKE $4
                    OR COALESCE(u.email, '') ILIKE $4
                    OR COALESCE(s.device_name, s.profile->>'_sessionDeviceName', '') ILIKE $4
                    OR COALESCE(s.device_id, s.profile->>'_sessionDeviceId', '') ILIKE $4)
           ),
           filtered AS (
             SELECT * FROM sessions WHERE ($5::text IS NULL OR status = $5)
           ),
           page AS (
             SELECT f.*, signin.platform, signin.app_version, signin.ip_address, signin.user_agent,
                    ended.outcome AS ended_outcome, ended.created_at AS ended_at,
                    ended.actor_user_id AS ended_by,
                    COALESCE(NULLIF(actor.display_name, ''), actor.email) AS ended_by_name
             FROM filtered f
             LEFT JOIN LATERAL (
               SELECT e.platform, e.app_version, e.ip_address, e.user_agent
               FROM identity.auth_login_events e
               WHERE e.session_id = f.id AND e.outcome = 'success'
               ORDER BY e.created_at LIMIT 1
             ) signin ON true
             LEFT JOIN LATERAL (
               SELECT e.outcome, e.created_at, e.actor_user_id
               FROM identity.auth_login_events e
               WHERE e.session_id = f.id AND e.outcome IN ('signed_out', 'revoked')
               ORDER BY e.created_at DESC LIMIT 1
             ) ended ON true
             LEFT JOIN identity.users actor ON actor.id::text = ended.actor_user_id
             ORDER BY f.created_at DESC, f.id
             LIMIT $6 OFFSET $7
           )
           SELECT
             (SELECT COALESCE(jsonb_agg(jsonb_build_object(
                 'id', p.id, 'userId', p.user_id, 'name', p.name, 'email', p.email,
                 'roles', to_jsonb(p.roles), 'deviceId', p.device_id, 'deviceName', p.device_name,
                 'platform', p.platform, 'appVersion', p.app_version, 'ipAddress', p.ip_address,
                 'userAgent', p.user_agent, 'status', p.status, 'revokedReason', p.revoked_reason,
                 'endedOutcome', p.ended_outcome, 'endedBy', p.ended_by, 'endedByName', p.ended_by_name,
                 'signedInAt', p.created_at, 'lastSeenAt', GREATEST(p.last_seen_at, p.rotated_at),
                 'expiresAt', p.expires_at, 'endedAt', COALESCE(p.ended_at, p.revoked_at)
               ) ORDER BY p.created_at DESC, p.id), '[]'::jsonb) FROM page p) AS sessions,
             (SELECT count(*) FROM filtered) AS total,
             (SELECT count(*) FROM sessions WHERE status = 'active') AS active,
             (SELECT count(*) FROM sessions WHERE status = 'revoked') AS revoked,
             (SELECT count(*) FROM sessions WHERE status = 'expired') AS expired"#,
    )
    .bind(tenant)
    .bind(from)
    .bind(to)
    .bind(pattern)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_one(&pool)
    .await?;
    let mut sessions: Value = row.try_get("sessions")?;
    if let Some(items) = sessions.as_array_mut() {
        for item in items {
            let Some(object) = item.as_object_mut() else {
                continue;
            };
            let status = object
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("active")
                .to_owned();
            let reason = end_reason(
                &status,
                object.get("revokedReason").and_then(Value::as_str),
                object.get("endedOutcome").and_then(Value::as_str),
            );
            object.insert("endReason".into(), json!(reason));
            object.insert(
                "current".into(),
                json!(
                    object.get("id").and_then(Value::as_str)
                        == Some(principal.session_id.to_string().as_str())
                ),
            );
            object.remove("revokedReason");
            object.remove("endedOutcome");
        }
    }
    Ok(Json(ApiResponse::new(json!({
        "sessions": sessions,
        "total": row.try_get::<i64, _>("total")?,
        "counts": {
            "active": row.try_get::<i64, _>("active")?,
            "revoked": row.try_get::<i64, _>("revoked")?,
            "expired": row.try_get::<i64, _>("expired")?,
        },
        "limit": limit,
        "offset": offset,
    }))))
}

async fn list_events(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, READ_PERMISSION)?;
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let tenant = tenant_uuid(&pool, &principal.student.tenant_id).await?;
    let (from, to) = day_bounds(query.from, query.to)?;
    let outcome = query.outcome.as_deref().map(str::trim).filter(|value| {
        matches!(
            *value,
            "success" | "failure" | "blocked" | "signed_out" | "revoked"
        )
    });
    let pattern = like_pattern(query.q.as_deref());
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).clamp(0, 100_000);
    let row = sqlx::query(
        r#"WITH events AS (
             SELECT e.*,
                    COALESCE(NULLIF(u.display_name, ''), u.email, e.email, '') AS name,
                    COALESCE(u.email, e.email, '') AS account_email,
                    COALESCE(NULLIF(actor.display_name, ''), actor.email) AS actor_name
             FROM identity.auth_login_events e
             LEFT JOIN identity.users u ON u.id::text = e.user_id
             LEFT JOIN identity.users actor ON actor.id::text = e.actor_user_id
             WHERE e.tenant_id = $1
               AND ($2::timestamptz IS NULL OR e.created_at >= $2)
               AND ($3::timestamptz IS NULL OR e.created_at < $3)
               AND ($4::text IS NULL
                    OR COALESCE(u.display_name, '') ILIKE $4
                    OR COALESCE(u.email, e.email, '') ILIKE $4
                    OR COALESCE(e.device_name, '') ILIKE $4
                    OR COALESCE(e.device_id, '') ILIKE $4
                    OR COALESCE(e.ip_address, '') ILIKE $4)
           ),
           filtered AS (
             SELECT * FROM events WHERE ($5::text IS NULL OR outcome = $5)
           )
           SELECT
             (SELECT COALESCE(jsonb_agg(jsonb_build_object(
                 'id', p.id, 'outcome', p.outcome, 'reason', p.reason, 'userId', p.user_id,
                 'name', p.name, 'email', p.account_email, 'sessionId', p.session_id,
                 'deviceId', p.device_id, 'deviceName', p.device_name, 'platform', p.platform,
                 'appVersion', p.app_version, 'ipAddress', p.ip_address, 'userAgent', p.user_agent,
                 'actorUserId', p.actor_user_id, 'actorName', p.actor_name, 'createdAt', p.created_at
               ) ORDER BY p.created_at DESC, p.id), '[]'::jsonb)
              FROM (SELECT * FROM filtered ORDER BY created_at DESC, id LIMIT $6 OFFSET $7) p) AS events,
             (SELECT count(*) FROM filtered) AS total,
             (SELECT count(*) FROM events WHERE outcome = 'success') AS successes,
             (SELECT count(*) FROM events WHERE outcome = 'failure') AS failures,
             (SELECT count(*) FROM events WHERE outcome = 'blocked') AS blocked,
             (SELECT count(*) FROM events WHERE outcome IN ('signed_out', 'revoked')) AS sign_outs"#,
    )
    .bind(tenant)
    .bind(from)
    .bind(to)
    .bind(pattern)
    .bind(outcome)
    .bind(limit)
    .bind(offset)
    .fetch_one(&pool)
    .await?;
    Ok(Json(ApiResponse::new(json!({
        "events": row.try_get::<Value, _>("events")?,
        "total": row.try_get::<i64, _>("total")?,
        "counts": {
            "success": row.try_get::<i64, _>("successes")?,
            "failure": row.try_get::<i64, _>("failures")?,
            "blocked": row.try_get::<i64, _>("blocked")?,
            "signedOut": row.try_get::<i64, _>("sign_outs")?,
        },
        "limit": limit,
        "offset": offset,
    }))))
}

/// Signs a member out of an active session in the administrator's tenant.
/// The session's device is told through the realtime channel and is rejected
/// on its next request.
async fn revoke_session(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, REVOKE_PERMISSION)?;
    if session_id == principal.session_id {
        return Err(ApiError::BadRequest(
            "This is your own session. Sign out from your profile instead.".into(),
        ));
    }
    let pool = control_pool(&state)?;
    ensure_schema(&pool).await;
    let tenant = tenant_uuid(&pool, &principal.student.tenant_id).await?;
    let revoked = sqlx::query(
        r#"UPDATE identity.auth_sessions
           SET revoked_at = now(),
               profile = jsonb_set(profile, '{_sessionRevokedReason}', '"admin_revoked"'::jsonb, true)
           WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL AND expires_at > now()
           RETURNING user_id,
                     COALESCE(device_id, profile->>'_sessionDeviceId') AS device_id,
                     COALESCE(device_name, profile->>'_sessionDeviceName') AS device_name"#,
    )
    .bind(session_id)
    .bind(tenant)
    .fetch_optional(&pool)
    .await?;
    let Some(revoked) = revoked else {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM identity.auth_sessions WHERE id=$1 AND tenant_id=$2)",
        )
        .bind(session_id)
        .bind(tenant)
        .fetch_one(&pool)
        .await?;
        return Err(if exists {
            ApiError::Conflict("This session has already ended".into())
        } else {
            ApiError::NotFound("Session not found".into())
        });
    };
    let user_id: String = revoked.try_get("user_id")?;
    // Drops the cached principal so the device is refused immediately.
    state
        .revoke_session(session_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    state.publish_realtime(
        RealtimePublication::tenant(
            &principal.student.tenant_id,
            "identity.session.replaced",
            json!({"sessionId": session_id, "reason": "revoked_by_admin"}),
        )
        .for_user(&user_id),
    );
    let device_name: Option<String> = revoked.try_get("device_name")?;
    record_in_background(
        &state,
        LoginEvent {
            outcome: "revoked",
            reason: Some("revoked_by_admin".into()),
            tenant_slug: Some(principal.student.tenant_id.clone()),
            user_id: Some(user_id.clone()),
            email: None,
            session_id: Some(session_id),
            device_id: revoked.try_get("device_id")?,
            device_name: device_name.clone(),
            actor_user_id: Some(principal.student.id.clone()),
        },
        // The request is the administrator's, not the revoked device's: keep
        // only where the revocation came from.
        ClientContext {
            ip_address: client_context(&headers, None).ip_address,
            ..ClientContext::default()
        },
    );
    Ok(Json(ApiResponse::new(json!({
        "id": session_id,
        "userId": user_id,
        "status": "revoked",
        "endReason": "revoked_by_admin",
        "endedAt": Utc::now(),
    }))))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use chrono::NaiveDate;

    use super::{client_context, day_bounds, end_reason, like_pattern, sanitize_version};

    #[test]
    fn client_context_prefers_forwarded_ip_and_explicit_platform() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("103.249.204.94, 10.0.0.2"),
        );
        headers.insert("x-client-platform", HeaderValue::from_static("Android"));
        headers.insert("x-app-version", HeaderValue::from_static("1.0.9+34"));
        let context = client_context(&headers, Some("iPhone or iPad"));
        assert_eq!(context.ip_address.as_deref(), Some("103.249.204.94"));
        assert_eq!(context.platform.as_deref(), Some("android"));
        assert_eq!(context.app_version.as_deref(), Some("1.0.9+34"));
    }

    #[test]
    fn client_context_infers_platform_and_rejects_bad_values() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        headers.insert("x-app-version", HeaderValue::from_static("1.0 <script>"));
        let context = client_context(&headers, Some("iPhone or iPad"));
        assert_eq!(context.ip_address, None);
        assert_eq!(context.platform.as_deref(), Some("ios"));
        assert_eq!(context.app_version, None);

        let mut headers = HeaderMap::new();
        headers.insert(
            "user-agent",
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0) Chrome/151.0"),
        );
        assert_eq!(
            client_context(&headers, None).platform.as_deref(),
            Some("web")
        );
        assert_eq!(sanitize_version(" 1.2.3 "), Some("1.2.3".into()));
    }

    #[test]
    fn end_reasons_distinguish_how_a_session_ended() {
        assert_eq!(end_reason("active", None, None), None);
        assert_eq!(end_reason("expired", None, None), Some("expired"));
        assert_eq!(
            end_reason("revoked", Some("admin_revoked"), None),
            Some("revoked_by_admin")
        );
        assert_eq!(
            end_reason("revoked", None, Some("signed_out")),
            Some("signed_out")
        );
        assert_eq!(
            end_reason("revoked", Some("signed_in_elsewhere"), None),
            Some("signed_in_elsewhere")
        );
        assert_eq!(
            end_reason("revoked", Some("replaced"), None),
            Some("signed_in_again")
        );
        assert_eq!(end_reason("revoked", None, None), Some("ended"));
    }

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern(Some("  ")), None);
        assert_eq!(like_pattern(Some("50%_off")), Some("%50\\%\\_off%".into()));
    }

    #[test]
    fn day_bounds_cover_whole_india_days() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let (from, to) = day_bounds(Some(day), Some(day)).unwrap();
        assert_eq!(from.unwrap().to_rfc3339(), "2026-09-28T18:30:00+00:00");
        assert_eq!(to.unwrap().to_rfc3339(), "2026-09-29T18:30:00+00:00");
        assert!(day_bounds(Some(day), day.pred_opt()).is_err());
    }
}
