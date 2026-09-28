//! Gate security: scanning passes at a checkpoint, the movement log, and
//! walk-in visitors.
//!
//! Three kinds of token arrive at the same scanner and the guard cannot tell
//! them apart by looking: an approved outpass or leave pass, a member's daily
//! campus-entry pass, or a visitor's card. Each has its own one-time lifecycle:
//!
//! * **outpass / leave pass** — one gate-out inside the approved window, then
//!   one gate-in (a late return is still let in, and flagged).
//! * **daily campus entry** — one gate-in per calendar day (campus time). It is
//!   never an exit pass; leaving campus needs an outpass or a leave pass.
//! * **visitor pass** (invited or walk-in) — one gate-in, then one gate-out.
//!
//! A repeated scan is refused with `409 already_scanned` and the earlier
//! movement, so the guard sees *when, where and by whom* it was used. The pass
//! row is locked (`FOR UPDATE`) for the whole decision, so two guards scanning
//! the same QR at the same moment cannot both succeed: the second waits, then
//! reads the first one's movement.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, FixedOffset, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{require, tenant_id, token_hash},
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/gatepass/scan", post(scan_gatepass))
        .route("/gatepass/movements", get(list_movements))
        .route("/gatepass/movements/{movement_id}", get(movement_detail))
        .route("/gatepass/visitors/walk-in", post(register_walk_in))
        .route(
            "/gatepass/visitors/{pass_id}/gate-out",
            post(visitor_gate_out),
        )
}

// ---------------------------------------------------------------------------
// Pure scan-state rules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Entry,
    Exit,
}

impl Direction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "entry" => Some(Self::Entry),
            "exit" => Some(Self::Exit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Exit => "exit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    /// Record the movement. `late` marks a return after the pass window.
    Allow { late: bool },
    /// The pass has already been used for this movement; report the latest
    /// recorded movement of the pass instead of recording a new one.
    AlreadyScanned,
    /// The pass exists but cannot be used for this movement right now.
    Refuse(String),
}

/// Outpass and leave pass: one gate-out inside the window, then one gate-in.
pub fn outpass_rule(
    direction: Direction,
    last: Option<Direction>,
    now: DateTime<Utc>,
    valid_from: DateTime<Utc>,
    valid_until: DateTime<Utc>,
) -> ScanOutcome {
    match (direction, last) {
        (Direction::Exit, None) => {
            if now < valid_from {
                ScanOutcome::Refuse(format!(
                    "This pass is valid for gate-out from {}",
                    campus_time(valid_from)
                ))
            } else if now > valid_until {
                ScanOutcome::Refuse(format!(
                    "This pass expired at {} without being used",
                    campus_time(valid_until)
                ))
            } else {
                ScanOutcome::Allow { late: false }
            }
        }
        (Direction::Entry, None) => ScanOutcome::Refuse(
            "This pass has no gate-out yet. Scan it at gate-out when the student leaves campus"
                .into(),
        ),
        (Direction::Entry, Some(Direction::Exit)) => ScanOutcome::Allow {
            late: now > valid_until,
        },
        // Out twice, in twice, or out again after the return: all replays.
        (_, Some(_)) => ScanOutcome::AlreadyScanned,
    }
}

/// Daily campus-entry pass: one gate-in per campus day, never an exit.
pub fn daily_access_rule(direction: Direction, entered_today: bool) -> ScanOutcome {
    match direction {
        Direction::Exit => ScanOutcome::Refuse(
            "A campus-entry pass is for gate-in only. Leaving campus needs an approved outpass or leave pass"
                .into(),
        ),
        Direction::Entry if entered_today => ScanOutcome::AlreadyScanned,
        Direction::Entry => ScanOutcome::Allow { late: false },
    }
}

/// Visitor pass (invited or walk-in): one gate-in inside the visit window
/// (30 minutes early grace), then one gate-out at any time.
pub fn visitor_rule(
    direction: Direction,
    state: &str,
    now: DateTime<Utc>,
    visit_from: DateTime<Utc>,
    visit_until: DateTime<Utc>,
) -> ScanOutcome {
    match (direction, state) {
        (_, "checked_out") => ScanOutcome::AlreadyScanned,
        (Direction::Entry, "checked_in") => ScanOutcome::AlreadyScanned,
        (Direction::Exit, "checked_in") => ScanOutcome::Allow { late: false },
        (Direction::Entry, "approved" | "sent" | "active") => {
            if now < visit_from - chrono::Duration::minutes(30) {
                ScanOutcome::Refuse(format!("This visit starts at {}", campus_time(visit_from)))
            } else if now > visit_until {
                ScanOutcome::Refuse(format!(
                    "This visitor pass expired at {}",
                    campus_time(visit_until)
                ))
            } else {
                ScanOutcome::Allow { late: false }
            }
        }
        (Direction::Exit, "approved" | "sent" | "active") => {
            ScanOutcome::Refuse("This visitor has not gated in yet".into())
        }
        _ => ScanOutcome::Refuse("This visitor pass is not active".into()),
    }
}

/// Campus wall-clock time (IST, which every tenant uses today).
fn campus_offset() -> FixedOffset {
    FixedOffset::east_opt(5 * 3600 + 30 * 60).expect("IST offset is valid")
}

pub fn campus_time(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&campus_offset())
        .format("%-I:%M %p, %-d %b")
        .to_string()
}

pub fn already_scanned_message(scanned_at: DateTime<Utc>, checkpoint: &str) -> String {
    format!(
        "Already scanned at {} by {}",
        campus_time(scanned_at),
        checkpoint
    )
}

// ---------------------------------------------------------------------------
// Walk-in validation (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalkInInput {
    pub visitor_name: String,
    pub visitor_phone: String,
    pub purpose: String,
    pub host_name: String,
    #[serde(default)]
    pub host_user_id: Option<String>,
    #[serde(default)]
    pub id_note: Option<String>,
    #[serde(default)]
    pub vehicle_number: Option<String>,
    pub checkpoint: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WalkIn {
    pub visitor_name: String,
    pub visitor_phone: String,
    pub purpose: String,
    pub host_name: String,
    pub host_user_id: Option<String>,
    pub id_note: Option<String>,
    pub vehicle_number: Option<String>,
    pub checkpoint: String,
}

fn optional_text(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|v| !v.is_empty())
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn validate_walk_in(input: &WalkInInput) -> Result<WalkIn, String> {
    let visitor_name = clean_text(&input.visitor_name);
    if visitor_name.chars().count() < 2 || visitor_name.chars().count() > 80 {
        return Err("Enter the visitor's full name".into());
    }
    let digits: String = input
        .visitor_phone
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let allowed = input
        .visitor_phone
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '+' | '-' | '(' | ')'));
    if !allowed || !(10..=15).contains(&digits.len()) {
        return Err("Enter a valid phone number (10 to 15 digits)".into());
    }
    let purpose = clean_text(&input.purpose);
    if purpose.is_empty() || purpose.chars().count() > 200 {
        return Err("Enter the purpose of the visit".into());
    }
    let host_name = clean_text(&input.host_name);
    if host_name.is_empty() || host_name.chars().count() > 120 {
        return Err("Enter whom the visitor is meeting".into());
    }
    let id_note = optional_text(&input.id_note);
    if id_note.as_ref().is_some_and(|v| v.chars().count() > 120) {
        return Err("Keep the ID note under 120 characters".into());
    }
    let vehicle_number = optional_text(&input.vehicle_number).map(|v| v.to_uppercase());
    if let Some(vehicle) = &vehicle_number
        && (vehicle.len() > 16
            || !vehicle
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-'))
    {
        return Err("Enter a valid vehicle number".into());
    }
    let checkpoint = clean_text(&input.checkpoint);
    if checkpoint.is_empty() || checkpoint.chars().count() > 60 {
        return Err("Choose the checkpoint".into());
    }
    Ok(WalkIn {
        visitor_name,
        visitor_phone: format!("+{digits}"),
        purpose,
        host_name,
        host_user_id: optional_text(&input.host_user_id),
        id_note,
        vehicle_number,
        checkpoint,
    })
}

// ---------------------------------------------------------------------------
// Shared SQL
// ---------------------------------------------------------------------------

/// One movement as the security portal shows it. `m` is gate_movements.
const MOVEMENT_JSON: &str = r#"jsonb_build_object(
    'id',m.id,'userId',m.user_id,'requestId',m.request_id,
    'visitorPassId',m.visitor_pass_id,'direction',m.direction,
    'checkpoint',m.checkpoint,'method',m.method,'createdAt',m.created_at,
    'passType',CASE
        WHEN r.id IS NOT NULL THEN r.pass_type
        WHEN v.id IS NOT NULL THEN CASE WHEN v.entry_mode='walk_in' THEN 'walk_in' ELSE 'visitor' END
        WHEN m.request_id IS NULL AND m.visitor_pass_id IS NULL THEN 'daily_access'
        ELSE 'unknown' END,
    'holderName',COALESCE(r.requester_name,v.visitor_name,student.full_name,holder.display_name,m.user_id),
    'rollNumber',student.student_number,
    'photoUrl',NULLIF(COALESCE(student.profile->>'photoUrl',holder.profile->>'photoUrl',''),''),
    'scannedById',m.scanned_by,
    'scannedByName',COALESCE(guard.display_name,m.scanned_by))"#;

const MOVEMENT_JOINS: &str = r#"
    LEFT JOIN campus_ops.gatepass_requests r
      ON r.tenant_id=m.tenant_id AND r.id=m.request_id
    LEFT JOIN campus_ops.visitor_passes v
      ON v.tenant_id=m.tenant_id AND v.id=m.visitor_pass_id
    LEFT JOIN identity.users holder
      ON m.visitor_pass_id IS NULL AND holder.id::text=m.user_id
    LEFT JOIN LATERAL (
      SELECT s.full_name,s.student_number,s.profile,s.department_id,s.email
        FROM core.students s
       WHERE s.tenant_id=m.tenant_id AND m.visitor_pass_id IS NULL
         AND s.user_account_id::text=m.user_id
       ORDER BY s.updated_at DESC LIMIT 1) student ON true
    LEFT JOIN identity.users guard ON guard.id::text=m.scanned_by"#;

async fn movement_json(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    movement_id: Uuid,
) -> ApiResult<Value> {
    let sql = format!(
        "SELECT {MOVEMENT_JSON} FROM campus_ops.gate_movements m {MOVEMENT_JOINS} \
         WHERE m.tenant_id=$1 AND m.id=$2"
    );
    sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(movement_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::NotFound("Gate movement not found".into()))
}

/// The 409 a replayed pass gets: the message, plus the movement it replays.
async fn already_scanned_response(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    previous_movement: Uuid,
) -> ApiResult<Response> {
    let previous = movement_json(tx, tenant, previous_movement).await?;
    let scanned_at = previous["createdAt"]
        .as_str()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let checkpoint = previous["checkpoint"].as_str().unwrap_or("the gate");
    let message = already_scanned_message(scanned_at, checkpoint);
    Ok((
        StatusCode::CONFLICT,
        Json(json!({
            "error": message,
            "code": "already_scanned",
            "details": previous,
        })),
    )
        .into_response())
}

#[allow(clippy::too_many_arguments)]
async fn insert_movement(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    user_id: &str,
    request_id: Option<Uuid>,
    visitor_pass_id: Option<Uuid>,
    direction: Direction,
    checkpoint: &str,
    scanned_by: &str,
    method: &str,
) -> ApiResult<Uuid> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO campus_ops.gate_movements
             (tenant_id,user_id,request_id,visitor_pass_id,direction,checkpoint,scanned_by,method)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id"#,
    )
    .bind(tenant)
    .bind(user_id)
    .bind(request_id)
    .bind(visitor_pass_id)
    .bind(direction.as_str())
    .bind(checkpoint)
    .bind(scanned_by)
    .bind(method)
    .fetch_one(&mut **tx)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn record_event(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    aggregate: &str,
    id: &str,
    event: &str,
    actor: &str,
    payload: &Value,
) -> ApiResult<()> {
    sqlx::query(
        r#"INSERT INTO campus_ops.events
             (tenant_id,module_key,aggregate_type,aggregate_id,event_type,actor_user_id,payload)
           VALUES($1,'gatepass',$2,$3,$4,$5,$6)"#,
    )
    .bind(tenant)
    .bind(aggregate)
    .bind(id)
    .bind(event)
    .bind(actor)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn publish(state: &AppState, tenant_slug: &str, aggregate: &str, id: &str, event: &str) {
    state.publish_realtime(RealtimePublication::tenant(
        tenant_slug,
        format!("gatepass.{event}"),
        json!({
            "module": "gatepass",
            "resource": aggregate,
            "resourceId": id,
            "operation": event,
            "invalidate": true,
        }),
    ));
}

fn clean_checkpoint(value: &str) -> ApiResult<String> {
    let checkpoint = clean_text(value);
    if checkpoint.is_empty() || checkpoint.chars().count() > 60 {
        return Err(ApiError::BadRequest("Choose the checkpoint".into()));
    }
    Ok(checkpoint)
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateScanRequest {
    qr_payload: String,
    direction: String,
    checkpoint: String,
}

enum MatchedPass {
    Request {
        id: Uuid,
        user_id: String,
        valid_from: DateTime<Utc>,
        valid_until: DateTime<Utc>,
        via_qr: bool,
    },
    Daily {
        user_id: String,
        via_qr: bool,
    },
    Visitor {
        id: Uuid,
        state: String,
        visit_from: DateTime<Utc>,
        visit_until: DateTime<Utc>,
    },
}

async fn find_pass(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    hash: &str,
) -> ApiResult<Option<MatchedPass>> {
    // A completed pass keeps its QR hash and manual code hash precisely so a
    // replay is recognised as "already scanned" instead of "unknown QR".
    if let Some((id, user_id, valid_from, valid_until, via_qr)) =
        sqlx::query_as::<_, (Uuid, String, DateTime<Utc>, DateTime<Utc>, bool)>(
            r#"SELECT id,requester_user_id,departure_at,return_at,
                  COALESCE(qr_token_hash=$2,false)
             FROM campus_ops.gatepass_requests
            WHERE tenant_id=$1 AND state IN ('approved','completed')
              AND (qr_token_hash=$2 OR manual_code_hash=$2)
            ORDER BY (state='approved') DESC, updated_at DESC
            LIMIT 1
            FOR UPDATE"#,
        )
        .bind(tenant)
        .bind(hash)
        .fetch_optional(&mut **tx)
        .await?
    {
        return Ok(Some(MatchedPass::Request {
            id,
            user_id,
            valid_from,
            valid_until,
            via_qr,
        }));
    }
    if let Some((user_id, via_qr)) = sqlx::query_as::<_, (String, bool)>(
        r#"SELECT user_id,qr_token_hash=$2
             FROM campus_ops.daily_access_passes
            WHERE tenant_id=$1 AND (qr_token_hash=$2 OR manual_code_hash=$2)
            LIMIT 1
            FOR UPDATE"#,
    )
    .bind(tenant)
    .bind(hash)
    .fetch_optional(&mut **tx)
    .await?
    {
        return Ok(Some(MatchedPass::Daily { user_id, via_qr }));
    }
    if let Some((id, state, visit_from, visit_until)) =
        sqlx::query_as::<_, (Uuid, String, DateTime<Utc>, DateTime<Utc>)>(
            r#"SELECT id,state,visit_from,visit_until
                 FROM campus_ops.visitor_passes
                WHERE tenant_id=$1 AND qr_token_hash=$2
                ORDER BY created_at DESC
                LIMIT 1
                FOR UPDATE"#,
        )
        .bind(tenant)
        .bind(hash)
        .fetch_optional(&mut **tx)
        .await?
    {
        return Ok(Some(MatchedPass::Visitor {
            id,
            state,
            visit_from,
            visit_until,
        }));
    }
    Ok(None)
}

async fn latest_movement(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    column: &str,
    id: Uuid,
) -> ApiResult<Option<(Uuid, String)>> {
    let sql = format!(
        "SELECT id,direction FROM campus_ops.gate_movements \
         WHERE tenant_id=$1 AND {column}=$2 ORDER BY created_at DESC LIMIT 1"
    );
    Ok(sqlx::query_as::<_, (Uuid, String)>(&sql)
        .bind(tenant)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?)
}

pub async fn scan_gatepass(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<GateScanRequest>,
) -> ApiResult<Response> {
    require(&access, "gatepass.scan.create")?;
    let direction = Direction::parse(input.direction.as_str())
        .ok_or_else(|| ApiError::BadRequest("Direction must be entry or exit".into()))?;
    let payload = input.qr_payload.trim();
    if payload.is_empty() {
        return Err(ApiError::BadRequest("Scan a gatepass QR first".into()));
    }
    let checkpoint = clean_checkpoint(&input.checkpoint)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    crate::visitors::ensure_visitor_pass_schema(db.pool(), &principal.student.tenant_id).await;
    let hash = token_hash(payload);
    let now = Utc::now();
    let mut tx = db.pool().begin().await?;

    let pass = find_pass(&mut tx, tenant, &hash)
        .await?
        .ok_or_else(|| ApiError::NotFound("This QR or code is not a valid pass".into()))?;

    let (movement_id, late) = match pass {
        MatchedPass::Request {
            id,
            user_id,
            valid_from,
            valid_until,
            via_qr,
        } => {
            let last = latest_movement(&mut tx, tenant, "request_id", id).await?;
            let last_direction = last.as_ref().and_then(|(_, d)| Direction::parse(d));
            match outpass_rule(direction, last_direction, now, valid_from, valid_until) {
                ScanOutcome::AlreadyScanned => {
                    let (previous, _) = last.expect("a replay has a previous movement");
                    return already_scanned_response(&mut tx, tenant, previous).await;
                }
                ScanOutcome::Refuse(message) => return Err(ApiError::Conflict(message)),
                ScanOutcome::Allow { late } => {
                    let movement = insert_movement(
                        &mut tx,
                        tenant,
                        &user_id,
                        Some(id),
                        None,
                        direction,
                        &checkpoint,
                        &principal.student.id,
                        if via_qr { "qr" } else { "manual_code" },
                    )
                    .await?;
                    if direction == Direction::Entry {
                        // Keep both hashes so a replay reads as "already
                        // scanned"; the pass is no longer shown to the student.
                        sqlx::query(
                            r#"UPDATE campus_ops.gatepass_requests
                                  SET state='completed',qr_payload=NULL,manual_code=NULL,
                                      updated_at=now()
                                WHERE tenant_id=$1 AND id=$2 AND state='approved'"#,
                        )
                        .bind(tenant)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    }
                    (movement, late)
                }
            }
        }
        MatchedPass::Daily { user_id, via_qr } => {
            let entered_today = sqlx::query_scalar::<_, Uuid>(
                r#"SELECT id FROM campus_ops.gate_movements
                    WHERE tenant_id=$1 AND user_id=$2 AND direction='entry'
                      AND request_id IS NULL AND visitor_pass_id IS NULL
                      AND (created_at AT TIME ZONE 'Asia/Kolkata')::date
                          =(now() AT TIME ZONE 'Asia/Kolkata')::date
                    ORDER BY created_at DESC LIMIT 1"#,
            )
            .bind(tenant)
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
            match daily_access_rule(direction, entered_today.is_some()) {
                ScanOutcome::AlreadyScanned => {
                    let previous = entered_today.expect("a replay has a previous movement");
                    return already_scanned_response(&mut tx, tenant, previous).await;
                }
                ScanOutcome::Refuse(message) => return Err(ApiError::Conflict(message)),
                ScanOutcome::Allow { late } => {
                    let movement = insert_movement(
                        &mut tx,
                        tenant,
                        &user_id,
                        None,
                        None,
                        direction,
                        &checkpoint,
                        &principal.student.id,
                        if via_qr { "qr" } else { "manual_code" },
                    )
                    .await?;
                    (movement, late)
                }
            }
        }
        MatchedPass::Visitor {
            id,
            state: pass_state,
            visit_from,
            visit_until,
        } => match visitor_rule(direction, &pass_state, now, visit_from, visit_until) {
            ScanOutcome::AlreadyScanned => {
                match latest_movement(&mut tx, tenant, "visitor_pass_id", id).await? {
                    Some((previous, _)) => {
                        return already_scanned_response(&mut tx, tenant, previous).await;
                    }
                    None => {
                        return Err(ApiError::Conflict(
                            "This visitor pass was already used".into(),
                        ));
                    }
                }
            }
            ScanOutcome::Refuse(message) => return Err(ApiError::Conflict(message)),
            ScanOutcome::Allow { late } => {
                let movement = insert_movement(
                    &mut tx,
                    tenant,
                    &id.to_string(),
                    None,
                    Some(id),
                    direction,
                    &checkpoint,
                    &principal.student.id,
                    "qr",
                )
                .await?;
                set_visitor_state(&mut tx, tenant, id, direction).await?;
                (movement, late)
            }
        },
    };

    let mut value = movement_json(&mut tx, tenant, movement_id).await?;
    if let Some(object) = value.as_object_mut() {
        object.insert("late".into(), json!(late));
    }
    let id = movement_id.to_string();
    record_event(
        &mut tx,
        tenant,
        "movement",
        &id,
        "movement.scanned",
        &principal.student.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    publish(
        &state,
        &principal.student.tenant_id,
        "movement",
        &id,
        "movement.scanned",
    );
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))).into_response())
}

async fn set_visitor_state(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    pass_id: Uuid,
    direction: Direction,
) -> ApiResult<()> {
    let sql = match direction {
        Direction::Entry => {
            r#"UPDATE campus_ops.visitor_passes
                  SET state='checked_in',checked_in_at=COALESCE(checked_in_at,now()),updated_at=now()
                WHERE tenant_id=$1 AND id=$2"#
        }
        Direction::Exit => {
            r#"UPDATE campus_ops.visitor_passes
                  SET state='checked_out',checked_out_at=now(),updated_at=now()
                WHERE tenant_id=$1 AND id=$2"#
        }
    };
    sqlx::query(sql)
        .bind(tenant)
        .bind(pass_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Movement log
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovementQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before: Option<DateTime<Utc>>,
}

pub async fn list_movements(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<MovementQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "gatepass.scan.read")?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    crate::visitors::ensure_visitor_pass_schema(db.pool(), &principal.student.tenant_id).await;
    let sql = format!(
        r#"SELECT jsonb_build_object(
             'movements',COALESCE((
               SELECT jsonb_agg(item.value ORDER BY item.created_at DESC)
                 FROM (SELECT {MOVEMENT_JSON} AS value, m.created_at
                         FROM campus_ops.gate_movements m {MOVEMENT_JOINS}
                        WHERE m.tenant_id=$1
                          AND ($3::timestamptz IS NULL OR m.created_at < $3)
                        ORDER BY m.created_at DESC
                        LIMIT $2) item),'[]'::jsonb),
             'today',(
               SELECT jsonb_build_object(
                 'entries',COUNT(*) FILTER (WHERE direction='entry'),
                 'exits',COUNT(*) FILTER (WHERE direction='exit'))
                 FROM campus_ops.gate_movements
                WHERE tenant_id=$1
                  AND (created_at AT TIME ZONE 'Asia/Kolkata')::date
                      =(now() AT TIME ZONE 'Asia/Kolkata')::date),
             'visitorsOnCampus',COALESCE((
               SELECT jsonb_agg(jsonb_build_object(
                 'id',v.id,'visitorName',v.visitor_name,'visitorPhone',v.visitor_phone,
                 'purpose',v.purpose,'hostName',v.host_name,
                 'entryMode',v.entry_mode,'vehicleNumber',v.vehicle_number,
                 'checkedInAt',v.checked_in_at,'visitUntil',v.visit_until)
                 ORDER BY v.checked_in_at DESC)
                 FROM campus_ops.visitor_passes v
                WHERE v.tenant_id=$1 AND v.state='checked_in'),'[]'::jsonb))"#
    );
    let data = sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(limit)
        .bind(query.before)
        .fetch_one(db.pool())
        .await?;
    Ok(Json(ApiResponse::new(data)))
}

pub async fn movement_detail(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(movement_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "gatepass.scan.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    crate::visitors::ensure_visitor_pass_schema(db.pool(), &principal.student.tenant_id).await;
    let sql = format!(
        r#"SELECT {MOVEMENT_JSON} || jsonb_build_object(
             'person',jsonb_build_object(
               'name',COALESCE(r.requester_name,v.visitor_name,student.full_name,holder.display_name,m.user_id),
               'userId',CASE WHEN m.visitor_pass_id IS NULL THEN m.user_id END,
               'rollNumber',student.student_number,
               'email',COALESCE(student.email,holder.email),
               'department',department.name,
               'phone',v.visitor_phone,
               'photoUrl',NULLIF(COALESCE(student.profile->>'photoUrl',holder.profile->>'photoUrl',''),'')),
             'pass',CASE
               WHEN r.id IS NOT NULL THEN jsonb_build_object(
                 'id',r.id,'type',r.pass_type,'state',r.state,'residency',r.residency,
                 'destination',r.destination,'reason',r.reason,
                 'validFrom',r.departure_at,'validUntil',r.return_at,
                 'approvedBy',COALESCE(approver.display_name,r.decided_by),
                 'decisionNote',r.decision_note)
               WHEN v.id IS NOT NULL THEN jsonb_build_object(
                 'id',v.id,'type',CASE WHEN v.entry_mode='walk_in' THEN 'walk_in' ELSE 'visitor' END,
                 'state',v.state,'purpose',v.purpose,'hostName',v.host_name,
                 'relationship',v.relationship,'vehicleNumber',v.vehicle_number,
                 'idNote',v.id_note,'validFrom',v.visit_from,'validUntil',v.visit_until,
                 'checkedInAt',v.checked_in_at,'checkedOutAt',v.checked_out_at,
                 'approvedBy',COALESCE(approver.display_name,v.decided_by),
                 'registeredBy',COALESCE(requester.display_name,v.requested_by))
               WHEN daily.id IS NOT NULL THEN jsonb_build_object(
                 'id',daily.id,'type','daily_access',
                 'validFrom',daily.activated_at,'activatedOn',daily.valid_on)
               ELSE NULL END,
             'timeline',COALESCE((
               SELECT jsonb_agg(jsonb_build_object(
                 'id',other.id,'direction',other.direction,'checkpoint',other.checkpoint,
                 'createdAt',other.created_at,
                 'scannedByName',COALESCE(other_guard.display_name,other.scanned_by))
                 ORDER BY other.created_at)
                 FROM campus_ops.gate_movements other
                 LEFT JOIN identity.users other_guard ON other_guard.id::text=other.scanned_by
                WHERE other.tenant_id=m.tenant_id AND (
                  (m.request_id IS NOT NULL AND other.request_id=m.request_id)
                  OR (m.visitor_pass_id IS NOT NULL AND other.visitor_pass_id=m.visitor_pass_id)
                  OR (m.request_id IS NULL AND m.visitor_pass_id IS NULL AND other.id=m.id))
             ),'[]'::jsonb))
           FROM campus_ops.gate_movements m {MOVEMENT_JOINS}
           LEFT JOIN core.departments department
             ON department.tenant_id=m.tenant_id AND department.id::text=student.department_id
           LEFT JOIN identity.users approver
             ON approver.id::text=COALESCE(r.decided_by,v.decided_by)
           LEFT JOIN identity.users requester ON requester.id::text=v.requested_by
           LEFT JOIN campus_ops.daily_access_passes daily
             ON m.request_id IS NULL AND m.visitor_pass_id IS NULL
            AND daily.tenant_id=m.tenant_id AND daily.user_id=m.user_id
          WHERE m.tenant_id=$1 AND m.id=$2"#
    );
    let data = sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(movement_id)
        .fetch_optional(db.pool())
        .await?
        .ok_or_else(|| ApiError::NotFound("Gate movement not found".into()))?;
    Ok(Json(ApiResponse::new(data)))
}

// ---------------------------------------------------------------------------
// Walk-in visitors
// ---------------------------------------------------------------------------

pub async fn register_walk_in(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<WalkInInput>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "gatepass.scan.create")?;
    let walk_in = validate_walk_in(&input).map_err(ApiError::BadRequest)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    crate::visitors::ensure_visitor_pass_schema(db.pool(), &principal.student.tenant_id).await;
    let mut tx = db.pool().begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant.to_string())
        .execute(&mut *tx)
        .await?;
    // A walk-in has no invitation, so there is no QR and nothing to deliver.
    // The visit runs to the end of the campus day; gate-out is recorded by the
    // guard from the portal.
    let pass_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO campus_ops.visitor_passes
             (tenant_id,visitor_kind,visitor_name,visitor_phone,purpose,relationship,
              host_user_id,host_name,requested_by,decided_by,visit_from,visit_until,
              state,delivery_state,entry_mode,vehicle_number,id_note,checked_in_at)
           VALUES($1,'guest',$2,$3,$4,'Walk-in visitor',$5,$6,$7,$7,now(),
                  GREATEST(
                    (((now() AT TIME ZONE 'Asia/Kolkata')::date + 1)::timestamp
                       AT TIME ZONE 'Asia/Kolkata'),
                    now() + interval '1 hour'),
                  'checked_in','not_configured','walk_in',$8,$9,now())
           RETURNING id"#,
    )
    .bind(tenant)
    .bind(&walk_in.visitor_name)
    .bind(&walk_in.visitor_phone)
    .bind(&walk_in.purpose)
    .bind(walk_in.host_user_id.as_deref().unwrap_or(""))
    .bind(&walk_in.host_name)
    .bind(&principal.student.id)
    .bind(walk_in.vehicle_number.as_deref())
    .bind(walk_in.id_note.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    let movement_id = insert_movement(
        &mut tx,
        tenant,
        &pass_id.to_string(),
        None,
        Some(pass_id),
        Direction::Entry,
        &walk_in.checkpoint,
        &principal.student.id,
        "walk_in",
    )
    .await?;
    let value = movement_json(&mut tx, tenant, movement_id).await?;
    let id = movement_id.to_string();
    record_event(
        &mut tx,
        tenant,
        "movement",
        &id,
        "visitor.walk_in",
        &principal.student.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    publish(
        &state,
        &principal.student.tenant_id,
        "movement",
        &id,
        "visitor.walk_in",
    );
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateOutInput {
    checkpoint: String,
}

pub async fn visitor_gate_out(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(pass_id): Path<Uuid>,
    Json(input): Json<GateOutInput>,
) -> ApiResult<Response> {
    require(&access, "gatepass.scan.create")?;
    let checkpoint = clean_checkpoint(&input.checkpoint)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    crate::visitors::ensure_visitor_pass_schema(db.pool(), &principal.student.tenant_id).await;
    let now = Utc::now();
    let mut tx = db.pool().begin().await?;
    let (pass_state, visit_from, visit_until) =
        sqlx::query_as::<_, (String, DateTime<Utc>, DateTime<Utc>)>(
            r#"SELECT state,visit_from,visit_until FROM campus_ops.visitor_passes
                WHERE tenant_id=$1 AND id=$2 FOR UPDATE"#,
        )
        .bind(tenant)
        .bind(pass_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::NotFound("Visitor pass not found".into()))?;
    match visitor_rule(Direction::Exit, &pass_state, now, visit_from, visit_until) {
        ScanOutcome::AlreadyScanned => {
            let previous = latest_movement(&mut tx, tenant, "visitor_pass_id", pass_id)
                .await?
                .ok_or_else(|| ApiError::Conflict("This visitor already left".into()))?;
            already_scanned_response(&mut tx, tenant, previous.0).await
        }
        ScanOutcome::Refuse(message) => Err(ApiError::Conflict(message)),
        ScanOutcome::Allow { .. } => {
            let movement_id = insert_movement(
                &mut tx,
                tenant,
                &pass_id.to_string(),
                None,
                Some(pass_id),
                Direction::Exit,
                &checkpoint,
                &principal.student.id,
                "manual",
            )
            .await?;
            set_visitor_state(&mut tx, tenant, pass_id, Direction::Exit).await?;
            let value = movement_json(&mut tx, tenant, movement_id).await?;
            let id = movement_id.to_string();
            record_event(
                &mut tx,
                tenant,
                "movement",
                &id,
                "movement.scanned",
                &principal.student.id,
                &value,
            )
            .await?;
            tx.commit().await?;
            publish(
                &state,
                &principal.student.tenant_id,
                "movement",
                &id,
                "movement.scanned",
            );
            Ok((StatusCode::CREATED, Json(ApiResponse::new(value))).into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 28, hour, 0, 0).unwrap()
    }

    #[test]
    fn outpass_is_one_exit_then_one_entry() {
        let (from, until) = (at(4), at(12));
        assert_eq!(
            outpass_rule(Direction::Exit, None, at(5), from, until),
            ScanOutcome::Allow { late: false }
        );
        assert_eq!(
            outpass_rule(Direction::Entry, Some(Direction::Exit), at(10), from, until),
            ScanOutcome::Allow { late: false }
        );
        assert_eq!(
            outpass_rule(
                Direction::Entry,
                Some(Direction::Entry),
                at(10),
                from,
                until
            ),
            ScanOutcome::AlreadyScanned
        );
        assert_eq!(
            outpass_rule(Direction::Exit, Some(Direction::Exit), at(6), from, until),
            ScanOutcome::AlreadyScanned
        );
        assert_eq!(
            outpass_rule(Direction::Exit, Some(Direction::Entry), at(11), from, until),
            ScanOutcome::AlreadyScanned
        );
        assert!(matches!(
            outpass_rule(Direction::Entry, None, at(5), from, until),
            ScanOutcome::Refuse(_)
        ));
    }

    #[test]
    fn outpass_window_bounds_the_exit_but_not_a_late_return() {
        let (from, until) = (at(4), at(12));
        assert!(matches!(
            outpass_rule(Direction::Exit, None, at(3), from, until),
            ScanOutcome::Refuse(_)
        ));
        assert!(matches!(
            outpass_rule(Direction::Exit, None, at(13), from, until),
            ScanOutcome::Refuse(_)
        ));
        assert_eq!(
            outpass_rule(Direction::Entry, Some(Direction::Exit), at(14), from, until),
            ScanOutcome::Allow { late: true }
        );
    }

    #[test]
    fn daily_access_is_one_gate_in_per_day_and_never_an_exit() {
        assert_eq!(
            daily_access_rule(Direction::Entry, false),
            ScanOutcome::Allow { late: false }
        );
        assert_eq!(
            daily_access_rule(Direction::Entry, true),
            ScanOutcome::AlreadyScanned
        );
        assert!(matches!(
            daily_access_rule(Direction::Exit, false),
            ScanOutcome::Refuse(_)
        ));
    }

    #[test]
    fn visitor_is_one_entry_then_one_exit() {
        let (from, until) = (at(4), at(12));
        assert_eq!(
            visitor_rule(Direction::Entry, "approved", at(5), from, until),
            ScanOutcome::Allow { late: false }
        );
        // 30 minutes early grace.
        assert_eq!(
            visitor_rule(
                Direction::Entry,
                "approved",
                from - Duration::minutes(20),
                from,
                until
            ),
            ScanOutcome::Allow { late: false }
        );
        assert!(matches!(
            visitor_rule(Direction::Entry, "approved", at(2), from, until),
            ScanOutcome::Refuse(_)
        ));
        assert!(matches!(
            visitor_rule(Direction::Entry, "approved", at(13), from, until),
            ScanOutcome::Refuse(_)
        ));
        assert!(matches!(
            visitor_rule(Direction::Exit, "approved", at(5), from, until),
            ScanOutcome::Refuse(_)
        ));
        assert_eq!(
            visitor_rule(Direction::Entry, "checked_in", at(5), from, until),
            ScanOutcome::AlreadyScanned
        );
        // An overstaying visitor can always be let out.
        assert_eq!(
            visitor_rule(Direction::Exit, "checked_in", at(20), from, until),
            ScanOutcome::Allow { late: false }
        );
        assert_eq!(
            visitor_rule(Direction::Exit, "checked_out", at(6), from, until),
            ScanOutcome::AlreadyScanned
        );
        assert_eq!(
            visitor_rule(Direction::Entry, "checked_out", at(6), from, until),
            ScanOutcome::AlreadyScanned
        );
        assert!(matches!(
            visitor_rule(Direction::Entry, "cancelled", at(5), from, until),
            ScanOutcome::Refuse(_)
        ));
    }

    #[test]
    fn already_scanned_message_names_time_and_checkpoint_in_campus_time() {
        let scanned = Utc.with_ymd_and_hms(2026, 9, 28, 5, 12, 0).unwrap();
        assert_eq!(
            already_scanned_message(scanned, "Main gate"),
            "Already scanned at 10:42 AM, 28 Sep by Main gate"
        );
    }

    fn walk_in() -> WalkInInput {
        WalkInInput {
            visitor_name: "  Ravi   Kumar ".into(),
            visitor_phone: "+91 98765-43210".into(),
            purpose: "Admission enquiry".into(),
            host_name: "Admissions office".into(),
            host_user_id: None,
            id_note: Some("  ".into()),
            vehicle_number: Some("tn 09 ab 1234".into()),
            checkpoint: "Main gate".into(),
        }
    }

    #[test]
    fn walk_in_input_is_normalised() {
        let value = validate_walk_in(&walk_in()).unwrap();
        assert_eq!(value.visitor_name, "Ravi Kumar");
        assert_eq!(value.visitor_phone, "+919876543210");
        assert_eq!(value.id_note, None);
        assert_eq!(value.vehicle_number.as_deref(), Some("TN 09 AB 1234"));
    }

    #[test]
    fn walk_in_input_is_validated() {
        let mut input = walk_in();
        input.visitor_name = "R".into();
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.visitor_phone = "12345".into();
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.visitor_phone = "98765abc43210".into();
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.purpose = " ".into();
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.host_name = String::new();
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.vehicle_number = Some("TN-09;DROP".into());
        assert!(validate_walk_in(&input).is_err());
        let mut input = walk_in();
        input.checkpoint = " ".into();
        assert!(validate_walk_in(&input).is_err());
    }
}
