//! Administrator push broadcasts: one message to chosen roles, people or
//! students.
//!
//! Every recipient gets a personal row in `campus_ops.notifications`, so a
//! broadcast always lands in the app's notification list. Phones are reached
//! by the notification worker, which turns queued rows into FCM HTTP v1 pushes
//! with its own Firebase service account. The API never sends a push and so
//! never needs that key: it queues a push for every recipient with a device
//! and the worker's delivery rows record what was actually sent. Only an
//! explicit `FCM_ENABLED=false` on the API records a broadcast as
//! `not_configured`.
//!
//! Recipients are resolved from the control plane's memberships (the same
//! `membership.roles` effective access is computed from) and mapped to the id
//! the inbox and the device registry use in the tenant database.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::tenant_id,
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub const READ_PERMISSION: &str = "notifications.broadcast.read";
pub const SEND_PERMISSION: &str = "notifications.broadcast.send";

/// Mirrors the CHECK constraints on `campus_ops.push_broadcasts`.
const TITLE_MAX_CHARS: usize = 120;
const BODY_MAX_CHARS: usize = 2000;
const IMAGE_URL_MAX_CHARS: usize = 2048;
const MAX_ROLES: usize = 50;
const MAX_USERS: usize = 20_000;
const HISTORY_DEFAULT: i64 = 30;
const HISTORY_MAX: i64 = 100;

/// Realtime event the app treats as "your inbox has something new".
pub const RECEIVED_EVENT: &str = "notification.broadcast.received";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/notifications/broadcasts",
            get(history).post(send_broadcast),
        )
        .route(
            "/notifications/broadcasts/audience-stats",
            get(audience_stats),
        )
        .route("/notifications/broadcasts/recipients", get(recipients))
        .route("/notifications/broadcasts/preview", post(preview))
}

// ---------------------------------------------------------------------------
// Pure rules (unit tested)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeviceCounts {
    android: i64,
    ios: i64,
    web: i64,
}

impl DeviceCounts {
    fn total(self) -> i64 {
        self.android + self.ios + self.web
    }

    fn add(&mut self, platform: &str, count: i64) {
        match platform {
            "android" => self.android += count,
            "ios" => self.ios += count,
            _ => self.web += count,
        }
    }

    fn json(self) -> Value {
        json!({"android": self.android, "ios": self.ios, "web": self.web})
    }
}

#[derive(Debug, Clone)]
struct Member {
    /// Control-plane account id: what the administrator picks and what
    /// realtime events are addressed to.
    account_id: String,
    /// Inbox and device-registry id in the tenant database.
    recipient_id: String,
    name: String,
    email: String,
    roles: Vec<String>,
    department: Option<String>,
    year: Option<String>,
    roll: Option<String>,
    devices: DeviceCounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PushReadiness {
    configured: bool,
    message: String,
}

/// Whether pushes should be queued for the notification worker.
///
/// The worker, not the API, holds the Firebase key and does the sending, so
/// the API's own copy of the key (or its absence) says nothing about delivery;
/// requiring it here is what left pushes unqueued in production while the
/// worker was ready. Only an explicit off switch stops queueing.
fn push_readiness(fcm_enabled: Option<&str>) -> PushReadiness {
    let disabled = fcm_enabled.map(str::trim).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    });
    if disabled {
        return PushReadiness {
            configured: false,
            message: "Push delivery is switched off on this server (FCM_ENABLED=false).                       Messages reach every recipient's in-app notification list only."
                .into(),
        };
    }
    PushReadiness {
        configured: true,
        message: "Pushes are queued for the notification worker, which delivers them                   through Firebase Cloud Messaging."
            .into(),
    }
}

fn environment_push_readiness() -> PushReadiness {
    push_readiness(std::env::var("FCM_ENABLED").ok().as_deref())
}

/// Year of study as a plain number ("1".."6"), from the shapes campus data
/// uses: `1`, `I`, `Year 1`, `1st year`. Academic years ("2026-27") are not
/// years of study and yield `None`.
fn normalize_year(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let stripped = lower
        .trim_start_matches("year")
        .trim()
        .trim_end_matches("year")
        .trim()
        .trim_end_matches("st")
        .trim_end_matches("nd")
        .trim_end_matches("rd")
        .trim_end_matches("th")
        .trim();
    let number = match stripped {
        "1" | "i" => 1,
        "2" | "ii" => 2,
        "3" | "iii" => 3,
        "4" | "iv" => 4,
        "5" | "v" => 5,
        "6" | "vi" => 6,
        _ => return None,
    };
    Some(number.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidMessage {
    title: String,
    body: String,
    image_url: Option<String>,
}

fn validate_message(
    title: &str,
    body: &str,
    image_url: Option<&str>,
) -> Result<ValidMessage, String> {
    let title = title.trim();
    let body = body.trim();
    if title.is_empty() {
        return Err("Add a title.".into());
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(format!(
            "Keep the title under {TITLE_MAX_CHARS} characters."
        ));
    }
    if body.is_empty() {
        return Err("Add a message.".into());
    }
    if body.chars().count() > BODY_MAX_CHARS {
        return Err(format!(
            "Keep the message under {BODY_MAX_CHARS} characters."
        ));
    }
    let image_url = match image_url.map(str::trim).filter(|url| !url.is_empty()) {
        None => None,
        Some(url) => {
            let valid = (url.starts_with("https://") || url.starts_with("http://"))
                && url.chars().count() <= IMAGE_URL_MAX_CHARS
                && !url.chars().any(char::is_whitespace);
            if !valid {
                return Err("The image link is not a valid web address.".into());
            }
            Some(url.to_owned())
        }
    };
    Ok(ValidMessage {
        title: title.to_owned(),
        body: body.to_owned(),
        image_url,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Audience {
    roles: Vec<String>,
    user_ids: Vec<String>,
}

/// Trims and de-duplicates the audience and rejects anything the tenant does
/// not have, so a typo can never silently shrink who is reached.
fn normalize_audience(
    roles: &[String],
    user_ids: &[String],
    known_roles: &HashSet<String>,
) -> Result<Audience, String> {
    if roles.len() > MAX_ROLES {
        return Err(format!("Choose at most {MAX_ROLES} roles."));
    }
    if user_ids.len() > MAX_USERS {
        return Err(format!("Choose at most {MAX_USERS} people."));
    }
    let mut audience = Audience::default();
    for role in roles {
        let role = role.trim();
        if role.is_empty() {
            continue;
        }
        if !known_roles.contains(role) {
            return Err(format!("\"{role}\" is not a role at this institution."));
        }
        if !audience.roles.iter().any(|known| known == role) {
            audience.roles.push(role.to_owned());
        }
    }
    let mut seen = HashSet::new();
    for user_id in user_ids {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            continue;
        }
        if Uuid::parse_str(user_id).is_err() {
            return Err("One of the chosen people is not a valid account.".into());
        }
        if seen.insert(user_id.to_ascii_lowercase()) {
            audience.user_ids.push(user_id.to_ascii_lowercase());
        }
    }
    if audience.roles.is_empty() && audience.user_ids.is_empty() {
        return Err("Choose at least one role, person or student.".into());
    }
    Ok(audience)
}

/// Everyone the audience reaches, once each, in directory order.
fn resolve<'a>(members: &'a [Member], audience: &Audience) -> Vec<&'a Member> {
    let users: HashSet<&str> = audience.user_ids.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    members
        .iter()
        .filter(|member| {
            users.contains(member.account_id.to_ascii_lowercase().as_str())
                || member
                    .roles
                    .iter()
                    .any(|role| audience.roles.iter().any(|chosen| chosen == role))
        })
        .filter(|member| seen.insert(member.recipient_id.clone()))
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Reach {
    recipients: i64,
    push_recipients: i64,
    devices: DeviceCounts,
}

fn reach(members: &[&Member]) -> Reach {
    let mut reach = Reach::default();
    for member in members {
        reach.recipients += 1;
        if member.devices.total() > 0 {
            reach.push_recipients += 1;
        }
        reach.devices.android += member.devices.android;
        reach.devices.ios += member.devices.ios;
        reach.devices.web += member.devices.web;
    }
    reach
}

impl Reach {
    fn json(self) -> Value {
        json!({
            "recipients": self.recipients,
            "pushRecipients": self.push_recipients,
            "withoutPush": self.recipients - self.push_recipients,
            "devices": self.devices.total(),
            "devicesByPlatform": self.devices.json(),
        })
    }
}

/// "Students, Captains and 3 people" — how the history names an audience.
fn audience_summary(audience: &Audience, role_names: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = audience
        .roles
        .iter()
        .map(|role| {
            role_names
                .get(role)
                .cloned()
                .unwrap_or_else(|| role.clone())
        })
        .collect();
    match audience.user_ids.len() {
        0 => {}
        1 => parts.push("1 person".into()),
        count => parts.push(format!("{count} people")),
    }
    match parts.len() {
        0 => String::new(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        }
    }
}

/// Per-recipient push state written on the inbox row. Only `queued` rows are
/// picked up by the notification worker, so a phone registered later never
/// receives a stale broadcast.
fn recipient_push_status(configured: bool, devices: DeviceCounts) -> &'static str {
    if !configured {
        "not_configured"
    } else if devices.total() == 0 {
        "no_device"
    } else {
        "queued"
    }
}

fn broadcast_push_status(configured: bool, reach: Reach) -> &'static str {
    if !configured {
        "not_configured"
    } else if reach.devices.total() == 0 {
        "no_devices"
    } else {
        "queued"
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Creates the broadcast table and grants the permissions on databases the
/// (stuck) migrator cannot reach. Mirrors
/// migrations/runtime/0125_push_broadcasts.sql; runs once per database per
/// process.
async fn ensure_push_broadcast_schema(pool: &sqlx::PgPool, key: &str) {
    use std::sync::{Mutex, OnceLock};
    static READY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let ready = READY.get_or_init(|| Mutex::new(HashSet::new()));
    if ready.lock().map(|set| set.contains(key)).unwrap_or(false) {
        return;
    }
    let applied = sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0125_push_broadcasts.sql"
    ))
    .execute(pool)
    .await;
    match applied {
        Ok(_) => {
            if let Ok(mut set) = ready.lock() {
                set.insert(key.to_owned());
            }
        }
        Err(error) => {
            tracing::warn!(%error, database = key, "push broadcast schema not applied");
        }
    }
}

struct Context {
    db: supercampus_database::Database,
    control: supercampus_database::Database,
    tenant: Uuid,
    tenant_slug: String,
}

async fn context(state: &AppState, principal: &AuthPrincipal) -> ApiResult<Context> {
    let tenant_slug = principal.student.tenant_id.clone();
    let db = state.tenant_database(&tenant_slug).await?;
    let control = state.database().unwrap_or_else(|| db.clone());
    ensure_push_broadcast_schema(db.pool(), &format!("tenant:{tenant_slug}")).await;
    ensure_push_broadcast_schema(control.pool(), "control").await;
    let tenant = tenant_id(db.pool(), &tenant_slug).await?;
    Ok(Context {
        db,
        control,
        tenant,
        tenant_slug,
    })
}

/// Active roles of the tenant: key -> display name, in name order.
async fn tenant_roles(ctx: &Context) -> ApiResult<Vec<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        r#"SELECT role.role_key, role.name
           FROM authz.roles role
           JOIN platform.tenants tenant ON tenant.id = role.tenant_id
           WHERE tenant.slug = $1 AND role.active
           ORDER BY role.name, role.role_key"#,
    )
    .bind(&ctx.tenant_slug)
    .fetch_all(ctx.control.pool())
    .await?)
}

type MemberRow = (
    String,
    String,
    String,
    Vec<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

type StudentRow = (
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

/// Every active member of the tenant with the facts the audience builder
/// filters on and the devices each can be pushed to.
async fn load_members(ctx: &Context) -> ApiResult<Vec<Member>> {
    let rows = sqlx::query_as::<_, MemberRow>(
        r#"SELECT account.id::text, lower(account.email), account.display_name,
                  membership.roles,
                  COALESCE(NULLIF(membership.profile->>'department',''),
                           NULLIF(membership.profile->>'dept',''),
                           NULLIF(account.profile->>'department',''),
                           NULLIF(account.profile->>'dept','')),
                  COALESCE(NULLIF(membership.profile->>'yearOfStudy',''),
                           NULLIF(membership.profile->>'year',''),
                           NULLIF(account.profile->>'yearOfStudy',''),
                           NULLIF(account.profile->>'year','')),
                  COALESCE(NULLIF(membership.profile->>'roll',''),
                           NULLIF(membership.profile->>'rollNumber',''),
                           NULLIF(account.profile->>'roll',''),
                           NULLIF(account.profile->>'rollNumber',''))
           FROM identity.tenant_memberships membership
           JOIN platform.tenants tenant ON tenant.id = membership.tenant_id
           JOIN identity.users account ON account.id = membership.user_id
           WHERE tenant.slug = $1 AND membership.active AND account.active
           ORDER BY account.display_name, account.email"#,
    )
    .bind(&ctx.tenant_slug)
    .fetch_all(ctx.control.pool())
    .await?;

    // The inbox and the device registry key users by the tenant database's
    // identity id where one exists for the email (see tenant_identity_user_id).
    let tenant_ids: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT lower(email), id::text FROM identity.users")
            .fetch_all(ctx.db.pool())
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "tenant identities unavailable for broadcast recipients");
                Vec::new()
            })
            .into_iter()
            .collect();

    // Students' department and year usually live on the student record.
    let students = sqlx::query_as::<_, StudentRow>(
        r#"SELECT student.user_account_id::text, lower(student.email), student.student_number,
                  COALESCE(NULLIF(department.code,''), NULLIF(student.profile->>'department','')),
                  COALESCE(NULLIF(student.profile->>'yearOfStudy',''),
                           NULLIF(student.profile->>'year',''),
                           NULLIF(student.academic_year,''))
           FROM core.students student
           LEFT JOIN core.departments department
             ON department.tenant_id = student.tenant_id
            AND department.id::text = student.department_id
           WHERE student.tenant_id = $1"#,
    )
    .bind(ctx.tenant)
    .fetch_all(ctx.db.pool())
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "student records unavailable for broadcast filters");
        Vec::new()
    });
    let mut student_by_account: HashMap<String, usize> = HashMap::new();
    let mut student_by_email: HashMap<String, usize> = HashMap::new();
    for (index, (account, email, ..)) in students.iter().enumerate() {
        if let Some(account) = account {
            student_by_account.insert(account.clone(), index);
        }
        if let Some(email) = email {
            student_by_email.insert(email.clone(), index);
        }
    }

    let mut devices: HashMap<String, DeviceCounts> = HashMap::new();
    for (user_id, platform, count) in sqlx::query_as::<_, (String, String, i64)>(
        r#"SELECT user_id, platform, count(*)
           FROM campus_ops.push_devices
           WHERE tenant_id = $1 AND enabled
           GROUP BY user_id, platform"#,
    )
    .bind(ctx.tenant)
    .fetch_all(ctx.db.pool())
    .await?
    {
        devices.entry(user_id).or_default().add(&platform, count);
    }

    Ok(rows
        .into_iter()
        .map(|(account_id, email, name, roles, department, year, roll)| {
            let recipient_id = tenant_ids
                .get(&email)
                .cloned()
                .unwrap_or_else(|| account_id.clone());
            let student = student_by_account
                .get(&account_id)
                .or_else(|| student_by_account.get(&recipient_id))
                .or_else(|| student_by_email.get(&email))
                .map(|index| &students[*index]);
            let department = student
                .and_then(|row| row.3.clone())
                .or(department)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            let year = year.as_deref().and_then(normalize_year).or_else(|| {
                student
                    .and_then(|row| row.4.as_deref())
                    .and_then(normalize_year)
            });
            let roll = roll
                .or_else(|| student.map(|row| row.2.clone()))
                .filter(|value| !value.trim().is_empty());
            let devices = devices
                .get(&recipient_id)
                .or_else(|| devices.get(&account_id))
                .copied()
                .unwrap_or_default();
            Member {
                account_id,
                recipient_id,
                name,
                email,
                roles,
                department,
                year,
                roll,
                devices,
            }
        })
        .collect())
}

fn require_read(access: &EffectiveAccess) -> ApiResult<()> {
    if access.allows(READ_PERMISSION) || access.allows(SEND_PERMISSION) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn require_send(access: &EffectiveAccess) -> ApiResult<()> {
    if access.allows(SEND_PERMISSION) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /notifications/broadcasts/audience-stats` — who can be reached, and
/// whether phones can be reached at all.
async fn audience_stats(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_read(&access)?;
    let ctx = context(&state, &principal).await?;
    let members = load_members(&ctx).await?;
    let roles = tenant_roles(&ctx).await?;
    let everyone: Vec<&Member> = members.iter().collect();
    let total = reach(&everyone);
    let role_stats: Vec<Value> = roles
        .iter()
        .map(|(key, name)| {
            let holders: Vec<&Member> = members
                .iter()
                .filter(|member| member.roles.iter().any(|role| role == key))
                .collect();
            let role_reach = reach(&holders);
            json!({
                "key": key,
                "name": name,
                "users": role_reach.recipients,
                "pushEnabledUsers": role_reach.push_recipients,
            })
        })
        .collect();
    let readiness = environment_push_readiness();
    Ok(Json(ApiResponse::new(json!({
        "totalUsers": total.recipients,
        "pushEnabledUsers": total.push_recipients,
        "noPushTokenUsers": total.recipients - total.push_recipients,
        "totalTokens": total.devices.total(),
        "tokensByPlatform": total.devices.json(),
        "roles": role_stats,
        "push": {
            "configured": readiness.configured,
            "provider": "fcm",
            "message": readiness.message,
        },
        "canSend": access.allows(SEND_PERMISSION),
    }))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipientsQuery {
    q: Option<String>,
    role: Option<String>,
    department: Option<String>,
    year: Option<String>,
    limit: Option<usize>,
}

fn member_json(member: &Member) -> Value {
    json!({
        "id": member.account_id,
        "name": member.name,
        "email": member.email,
        "roles": member.roles,
        "department": member.department,
        "year": member.year,
        "roll": member.roll,
        "pushEnabled": member.devices.total() > 0,
        "devices": member.devices.total(),
    })
}

fn matches_query(member: &Member, query: &RecipientsQuery) -> bool {
    let text = query
        .q
        .as_deref()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(text) = text {
        let hit = member.name.to_lowercase().contains(&text)
            || member.email.contains(&text)
            || member
                .roll
                .as_deref()
                .is_some_and(|roll| roll.to_lowercase().contains(&text));
        if !hit {
            return false;
        }
    }
    if let Some(role) = query.role.as_deref().filter(|role| !role.trim().is_empty())
        && !member.roles.iter().any(|held| held == role.trim())
    {
        return false;
    }
    if let Some(department) = query
        .department
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        && member.department.as_deref() != Some(department.trim())
    {
        return false;
    }
    if let Some(year) = query
        .year
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        && member.year.as_deref() != normalize_year(year).as_deref()
    {
        return false;
    }
    true
}

/// `GET /notifications/broadcasts/recipients` — the directory the audience
/// builder searches, with the department and year facets it filters by.
async fn recipients(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<RecipientsQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_send(&access)?;
    let ctx = context(&state, &principal).await?;
    let members = load_members(&ctx).await?;
    let limit = query.limit.unwrap_or(MAX_USERS).clamp(1, MAX_USERS);
    let mut departments = BTreeMap::<String, i64>::new();
    let mut years = BTreeMap::<String, i64>::new();
    for member in &members {
        if let Some(department) = &member.department {
            *departments.entry(department.clone()).or_default() += 1;
        }
        if let Some(year) = &member.year {
            *years.entry(year.clone()).or_default() += 1;
        }
    }
    let matching: Vec<&Member> = members
        .iter()
        .filter(|member| matches_query(member, &query))
        .collect();
    let users: Vec<Value> = matching
        .iter()
        .take(limit)
        .map(|m| member_json(m))
        .collect();
    Ok(Json(ApiResponse::new(json!({
        "users": users,
        "total": matching.len(),
        "departments": departments.keys().collect::<Vec<_>>(),
        "years": years.keys().collect::<Vec<_>>(),
    }))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudienceInput {
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    user_ids: Vec<String>,
}

/// `POST /notifications/broadcasts/preview` — exactly who a send would reach.
async fn preview(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<AudienceInput>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_send(&access)?;
    let ctx = context(&state, &principal).await?;
    let roles = tenant_roles(&ctx).await?;
    let known: HashSet<String> = roles.iter().map(|(key, _)| key.clone()).collect();
    let audience =
        normalize_audience(&input.roles, &input.user_ids, &known).map_err(ApiError::BadRequest)?;
    let members = load_members(&ctx).await?;
    let resolved = resolve(&members, &audience);
    let readiness = environment_push_readiness();
    let mut body = reach(&resolved).json();
    body["pushConfigured"] = json!(readiness.configured);
    body["pushMessage"] = json!(readiness.message);
    Ok(Json(ApiResponse::new(body)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BroadcastInput {
    title: String,
    body: String,
    image_url: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    user_ids: Vec<String>,
}

/// `POST /notifications/broadcasts` — put the message in every recipient's
/// inbox and queue the pushes.
async fn send_broadcast(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<BroadcastInput>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_send(&access)?;
    let message = validate_message(&input.title, &input.body, input.image_url.as_deref())
        .map_err(ApiError::BadRequest)?;
    let ctx = context(&state, &principal).await?;
    let roles = tenant_roles(&ctx).await?;
    let known: HashSet<String> = roles.iter().map(|(key, _)| key.clone()).collect();
    let role_names: HashMap<String, String> = roles.into_iter().collect();
    let audience =
        normalize_audience(&input.roles, &input.user_ids, &known).map_err(ApiError::BadRequest)?;
    let members = load_members(&ctx).await?;
    let resolved = resolve(&members, &audience);
    if resolved.is_empty() {
        return Err(ApiError::BadRequest(
            "No active account matches this audience.".into(),
        ));
    }
    let readiness = environment_push_readiness();
    let totals = reach(&resolved);
    let push_status = broadcast_push_status(readiness.configured, totals);
    let summary = audience_summary(&audience, &role_names);

    let mut tx = ctx.db.pool().begin().await?;
    let (broadcast_id, created_at) = sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
        r#"INSERT INTO campus_ops.push_broadcasts
             (tenant_id,title,body,image_url,audience,audience_summary,recipient_count,
              push_recipient_count,device_count,push_status,sent_by_user_id,sent_by_name,
              sent_by_email)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
           RETURNING id, created_at"#,
    )
    .bind(ctx.tenant)
    .bind(&message.title)
    .bind(&message.body)
    .bind(message.image_url.as_deref())
    .bind(json!({"roles": audience.roles, "userIds": audience.user_ids}))
    .bind(&summary)
    .bind(i32::try_from(totals.recipients).unwrap_or(i32::MAX))
    .bind(i32::try_from(totals.push_recipients).unwrap_or(i32::MAX))
    .bind(i32::try_from(totals.devices.total()).unwrap_or(i32::MAX))
    .bind(push_status)
    .bind(&principal.student.id)
    .bind(&principal.student.name)
    .bind(&principal.student.email)
    .fetch_one(&mut *tx)
    .await?;

    let mut data = json!({
        "broadcastId": broadcast_id,
        "sentBy": principal.student.name,
    });
    if let Some(image_url) = &message.image_url {
        data["imageUrl"] = json!(image_url);
    }
    let recipient_ids: Vec<String> = resolved.iter().map(|m| m.recipient_id.clone()).collect();
    let statuses: Vec<String> = resolved
        .iter()
        .map(|m| recipient_push_status(readiness.configured, m.devices).to_owned())
        .collect();
    let inserted = sqlx::query(
        r#"INSERT INTO campus_ops.notifications
             (tenant_id,recipient_user_id,category,event_type,title,body,data,priority,
              requires_action,deep_link,deduplication_key,push_status)
           SELECT $1, recipient.user_id, 'broadcast', 'broadcast.sent', $2, $3, $4, 'high',
                  false, NULL, 'broadcast:' || $5::text || ':' || recipient.user_id,
                  recipient.push_status
           FROM unnest($6::text[], $7::text[]) AS recipient(user_id, push_status)
           ON CONFLICT (tenant_id, deduplication_key)
             WHERE deduplication_key IS NOT NULL DO NOTHING"#,
    )
    .bind(ctx.tenant)
    .bind(&message.title)
    .bind(&message.body)
    .bind(&data)
    .bind(broadcast_id.to_string())
    .bind(&recipient_ids)
    .bind(&statuses)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;

    for member in &resolved {
        state.publish_realtime(
            RealtimePublication::tenant(
                ctx.tenant_slug.clone(),
                RECEIVED_EVENT,
                json!({"broadcastId": broadcast_id}),
            )
            .for_user(member.account_id.clone()),
        );
    }
    tracing::info!(
        tenant = %ctx.tenant_slug,
        %broadcast_id,
        recipients = totals.recipients,
        devices = totals.devices.total(),
        push_status,
        "push broadcast sent"
    );

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::new(json!({
            "id": broadcast_id,
            "title": message.title,
            "body": message.body,
            "imageUrl": message.image_url,
            "audienceSummary": summary,
            "audience": {"roles": audience.roles, "userIds": audience.user_ids},
            "createdAt": created_at,
            "sentBy": {"id": principal.student.id, "name": principal.student.name,
                       "email": principal.student.email},
            "inAppDelivered": inserted,
            "reach": totals.json(),
            "push": {
                "status": push_status,
                "configured": readiness.configured,
                "message": readiness.message,
            },
        }))),
    ))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

/// `GET /notifications/broadcasts` — what was sent, by whom, to whom, and how
/// far it got.
async fn history(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_read(&access)?;
    let ctx = context(&state, &principal).await?;
    let limit = query.limit.unwrap_or(HISTORY_DEFAULT).clamp(1, HISTORY_MAX);
    let rows = sqlx::query(
        r#"SELECT broadcast.id, broadcast.title, broadcast.body, broadcast.image_url,
                  broadcast.audience, broadcast.audience_summary, broadcast.recipient_count,
                  broadcast.push_recipient_count, broadcast.device_count, broadcast.push_status,
                  broadcast.sent_by_name, broadcast.sent_by_email, broadcast.created_at,
                  COALESCE(inbox.read_count, 0) AS read_count,
                  COALESCE(push.sent, 0) AS push_sent,
                  COALESCE(push.failed, 0) AS push_failed,
                  COALESCE(push.pending, 0) AS push_pending
           FROM campus_ops.push_broadcasts broadcast
           LEFT JOIN LATERAL (
             SELECT count(*) FILTER (WHERE notification.read_at IS NOT NULL) AS read_count
             FROM campus_ops.notifications notification
             WHERE notification.tenant_id = broadcast.tenant_id
               AND notification.category = 'broadcast'
               AND notification.data->>'broadcastId' = broadcast.id::text
           ) inbox ON true
           LEFT JOIN LATERAL (
             SELECT count(*) FILTER (WHERE delivery.status = 'sent') AS sent,
                    count(*) FILTER (WHERE delivery.status IN ('failed','invalid')) AS failed,
                    count(*) FILTER (
                      WHERE delivery.status IN ('queued','processing','retrying')) AS pending
             FROM campus_ops.notifications notification
             JOIN campus_ops.notification_push_deliveries delivery
               ON delivery.tenant_id = notification.tenant_id
              AND delivery.notification_id = notification.id
             WHERE notification.tenant_id = broadcast.tenant_id
               AND notification.category = 'broadcast'
               AND notification.data->>'broadcastId' = broadcast.id::text
           ) push ON true
           WHERE broadcast.tenant_id = $1
           ORDER BY broadcast.created_at DESC
           LIMIT $2"#,
    )
    .bind(ctx.tenant)
    .bind(limit)
    .fetch_all(ctx.db.pool())
    .await?;
    let broadcasts = rows
        .iter()
        .map(history_json)
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(ApiResponse::new(json!({ "broadcasts": broadcasts }))))
}

fn history_json(row: &sqlx::postgres::PgRow) -> Result<Value, sqlx::Error> {
    use sqlx::Row;
    let recipients: i32 = row.try_get("recipient_count")?;
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?,
        "title": row.try_get::<String, _>("title")?,
        "body": row.try_get::<String, _>("body")?,
        "imageUrl": row.try_get::<Option<String>, _>("image_url")?,
        "audience": row.try_get::<Value, _>("audience")?,
        "audienceSummary": row.try_get::<String, _>("audience_summary")?,
        "createdAt": row.try_get::<DateTime<Utc>, _>("created_at")?,
        "sentBy": {
            "name": row.try_get::<String, _>("sent_by_name")?,
            "email": row.try_get::<Option<String>, _>("sent_by_email")?,
        },
        "recipients": recipients,
        "inAppDelivered": recipients,
        "read": row.try_get::<i64, _>("read_count")?,
        "push": {
            "status": row.try_get::<String, _>("push_status")?,
            "recipients": row.try_get::<i32, _>("push_recipient_count")?,
            "devices": row.try_get::<i32, _>("device_count")?,
            "sent": row.try_get::<i64, _>("push_sent")?,
            "failed": row.try_get::<i64, _>("push_failed")?,
            "pending": row.try_get::<i64, _>("push_pending")?,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(account: &str, recipient: &str, roles: &[&str], android: i64) -> Member {
        Member {
            account_id: account.into(),
            recipient_id: recipient.into(),
            name: account.into(),
            email: format!("{account}@example.test"),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            department: None,
            year: None,
            roll: None,
            devices: DeviceCounts {
                android,
                ..DeviceCounts::default()
            },
        }
    }

    fn known(roles: &[&str]) -> HashSet<String> {
        roles.iter().map(|role| (*role).to_owned()).collect()
    }

    const A: &str = "00000000-0000-0000-0000-00000000000a";
    const B: &str = "00000000-0000-0000-0000-00000000000b";
    const C: &str = "00000000-0000-0000-0000-00000000000c";

    #[test]
    fn pushes_are_queued_unless_explicitly_switched_off() {
        // The worker holds the Firebase key; the API queues without it.
        assert!(push_readiness(None).configured);
        assert!(push_readiness(Some("true")).configured);
        assert!(push_readiness(Some("1")).configured);
        assert!(!push_readiness(Some("false")).configured);
        assert!(!push_readiness(Some(" FALSE ")).configured);
        assert!(!push_readiness(Some("0")).configured);
        assert!(!push_readiness(Some("off")).configured);
    }

    #[test]
    fn years_of_study_are_normalized_and_academic_years_ignored() {
        assert_eq!(normalize_year("I").as_deref(), Some("1"));
        assert_eq!(normalize_year("Year 2").as_deref(), Some("2"));
        assert_eq!(normalize_year("3rd year").as_deref(), Some("3"));
        assert_eq!(normalize_year(" iv ").as_deref(), Some("4"));
        assert_eq!(normalize_year("2026-27"), None);
        assert_eq!(normalize_year(""), None);
    }

    #[test]
    fn a_message_needs_a_title_and_body_and_a_web_image() {
        assert!(validate_message(" ", "Body", None).is_err());
        assert!(validate_message("Title", "  ", None).is_err());
        assert!(validate_message(&"x".repeat(121), "Body", None).is_err());
        assert!(validate_message("Title", &"x".repeat(2001), None).is_err());
        assert!(validate_message("Title", "Body", Some("javascript:alert(1)")).is_err());
        assert!(validate_message("Title", "Body", Some("https://a.test/x y.png")).is_err());
        let valid =
            validate_message(" Exam ", " Hall 3 ", Some(" https://cdn.test/a.png ")).unwrap();
        assert_eq!(valid.title, "Exam");
        assert_eq!(valid.body, "Hall 3");
        assert_eq!(valid.image_url.as_deref(), Some("https://cdn.test/a.png"));
        assert_eq!(
            validate_message("T", "B", Some("  ")).unwrap().image_url,
            None
        );
    }

    #[test]
    fn an_audience_must_name_someone_the_tenant_has() {
        let roles = known(&["student", "captain"]);
        assert!(normalize_audience(&[], &[], &roles).is_err());
        assert!(normalize_audience(&["dean".into()], &[], &roles).is_err());
        assert!(normalize_audience(&[], &["not-a-uuid".into()], &roles).is_err());
        let audience = normalize_audience(
            &[" student ".into(), "student".into()],
            &[A.to_uppercase(), A.into()],
            &roles,
        )
        .unwrap();
        assert_eq!(audience.roles, vec!["student".to_owned()]);
        assert_eq!(audience.user_ids, vec![A.to_owned()]);
    }

    #[test]
    fn roles_and_people_resolve_to_each_person_once() {
        let members = vec![
            member(A, "tenant-a", &["student"], 1),
            member(B, "tenant-b", &["captain", "student"], 0),
            member(C, "tenant-c", &["staff"], 2),
        ];
        let audience = Audience {
            roles: vec!["student".into()],
            user_ids: vec![B.into(), C.into()],
        };
        let resolved = resolve(&members, &audience);
        let ids: Vec<&str> = resolved.iter().map(|m| m.recipient_id.as_str()).collect();
        assert_eq!(ids, vec!["tenant-a", "tenant-b", "tenant-c"]);
        let totals = reach(&resolved);
        assert_eq!(totals.recipients, 3);
        assert_eq!(totals.push_recipients, 2);
        assert_eq!(totals.devices.total(), 3);

        let captains = Audience {
            roles: vec!["captain".into()],
            user_ids: vec![],
        };
        assert_eq!(resolve(&members, &captains).len(), 1);
    }

    #[test]
    fn push_state_is_honest_about_what_will_happen() {
        let with_phone = DeviceCounts {
            android: 1,
            ..DeviceCounts::default()
        };
        assert_eq!(recipient_push_status(false, with_phone), "not_configured");
        assert_eq!(
            recipient_push_status(true, DeviceCounts::default()),
            "no_device"
        );
        assert_eq!(recipient_push_status(true, with_phone), "queued");
        let none = Reach::default();
        let some = Reach {
            recipients: 1,
            push_recipients: 1,
            devices: with_phone,
        };
        assert_eq!(broadcast_push_status(false, some), "not_configured");
        assert_eq!(broadcast_push_status(true, none), "no_devices");
        assert_eq!(broadcast_push_status(true, some), "queued");
    }

    #[test]
    fn audiences_are_summarized_in_plain_words() {
        let names = HashMap::from([
            ("student".to_owned(), "Student".to_owned()),
            ("captain".to_owned(), "Vendor Shop Captain".to_owned()),
        ]);
        let summary = |roles: &[&str], users: usize| {
            audience_summary(
                &Audience {
                    roles: roles.iter().map(|r| (*r).to_owned()).collect(),
                    user_ids: vec![A.to_owned(); users],
                },
                &names,
            )
        };
        assert_eq!(summary(&["student"], 0), "Student");
        assert_eq!(summary(&[], 1), "1 person");
        assert_eq!(
            summary(&["student", "captain"], 3),
            "Student, Vendor Shop Captain and 3 people"
        );
    }

    #[test]
    fn recipient_search_matches_name_roll_and_filters() {
        let mut student = member(A, "tenant-a", &["student"], 0);
        student.name = "Priya Raman".into();
        student.roll = Some("21CS042".into());
        student.department = Some("CSE".into());
        student.year = Some("2".into());
        let query =
            |q: Option<&str>, department: Option<&str>, year: Option<&str>| RecipientsQuery {
                q: q.map(str::to_owned),
                role: Some("student".into()),
                department: department.map(str::to_owned),
                year: year.map(str::to_owned),
                limit: None,
            };
        assert!(matches_query(&student, &query(Some("priya"), None, None)));
        assert!(matches_query(
            &student,
            &query(Some("cs042"), Some("CSE"), Some("II"))
        ));
        assert!(!matches_query(&student, &query(None, Some("IT"), None)));
        assert!(!matches_query(&student, &query(None, None, Some("3"))));
        assert!(!matches_query(&student, &query(Some("arun"), None, None)));
    }
}
