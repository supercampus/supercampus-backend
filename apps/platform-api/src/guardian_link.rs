//! Parent approval of an outpass, without an account.
//!
//! An outpass runs `["parent", "warden", "security"]`, and the first approver
//! has no login and never will. A guardian is reached on WhatsApp, taps a link,
//! and answers — no app, no password, no enrolment.
//!
//! The link is the whole authorisation, so it is kept as narrow as a session
//! would be:
//!
//! * 256 bits of randomness, and only its hash is stored;
//! * good for one decision on one request, at one named step;
//! * spent the first time it is used, and expired at the departure time.
//!
//! The decision itself goes through the same
//! [`crate::operations::advance_gatepass_step`] the staff endpoint uses. Two
//! implementations of "what approval does to an outpass" would drift.

use axum::{
    Json,
    extract::{Path, State},
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{advance_gatepass_step, token_hash},
    state::AppState,
};
use supercampus_notifications::whatsapp::{DeliveryOutcome, WhatsAppMessage};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuardianDecisionInput {
    /// `approved` or `rejected`.
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Mints a link and sends it to the guardian.
///
/// Called when a hosteller raises an outpass. Failure to deliver is recorded
/// but never fails the request: the pass is validly raised either way, and a
/// student should not have their outpass rejected because a phone was off.
pub async fn issue_guardian_link(
    state: &AppState,
    tenant_slug: &str,
    tenant: Uuid,
    pool: &sqlx::PgPool,
    request_id: Uuid,
    guardian_name: &str,
    guardian_phone: &str,
    student_name: &str,
    departure_at: DateTime<Utc>,
) -> ApiResult<Value> {
    let raw = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let hash = token_hash(&raw);
    // The link dies when the trip starts. An approval that arrives after the
    // student was due to leave is not an approval, it is a liability.
    let expires_at = departure_at.max(Utc::now() + Duration::hours(1));

    sqlx::query(
        r#"INSERT INTO campus_ops.guardian_approval_tokens
               (tenant_id, request_id, step_key, guardian_name, guardian_phone,
                token_hash, expires_at)
           VALUES ($1,$2,'parent',$3,$4,$5,$6)"#,
    )
    .bind(tenant)
    .bind(request_id)
    .bind(guardian_name)
    .bind(guardian_phone)
    .bind(&hash)
    .bind(expires_at)
    .execute(pool)
    .await?;

    let link = format!("{}/gatepass/approve/{raw}", crate::public_base_url());
    let body = format!(
        "{guardian_name}, {student_name} has requested an outpass. Approve or decline here: {link}"
    );

    let outcome = state
        .whatsapp()
        .send(WhatsAppMessage {
            to: guardian_phone.to_owned(),
            body,
            media_url: None,
            template_variables: vec![student_name.to_owned(), link.clone()],
        })
        .await;

    let (delivery_state, delivery_error) = match outcome {
        Ok(DeliveryOutcome::Sent { .. }) => ("sent", None),
        Ok(DeliveryOutcome::NotConfigured) => ("not_configured", None),
        Err(error) => {
            tracing::error!(error = ?error, %request_id, "guardian approval link could not be sent");
            ("failed", Some(error.to_string()))
        }
    };

    sqlx::query(
        r#"UPDATE campus_ops.guardian_approval_tokens
           SET delivery_state = $3, delivery_error = $4
           WHERE tenant_id = $1 AND token_hash = $2"#,
    )
    .bind(tenant)
    .bind(&hash)
    .bind(delivery_state)
    .bind(delivery_error.as_deref())
    .execute(pool)
    .await?;

    let _ = tenant_slug;
    Ok(json!({
        "guardianName": guardian_name,
        "guardianPhone": guardian_phone,
        "deliveryState": delivery_state,
        "expiresAt": expires_at,
    }))
}

/// What the guardian sees before deciding.
///
/// Unauthenticated by design, so it discloses only what someone already holding
/// the link needs in order to answer: which child, going where, and when. No
/// roll number, no contact details, nothing about anyone else.
pub async fn show_guardian_request(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let (_, _, value) = resolve(&state, &token).await?;
    Ok(Json(ApiResponse::new(value)))
}

/// Records the guardian's answer and advances the pass to the warden.
pub async fn decide_as_guardian(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(input): Json<GuardianDecisionInput>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !matches!(input.decision.as_str(), "approved" | "rejected") {
        return Err(ApiError::BadRequest(
            "A decision is either approved or rejected".into(),
        ));
    }

    let (tenant_slug, token_row, _) = resolve(&state, &token).await?;
    let db = state.tenant_database(&tenant_slug).await?;
    let mut tx = db.pool().begin().await?;

    // Spend the token inside the same transaction that records the decision,
    // and only if it is still unspent. Two taps on the same link — which is
    // exactly what a WhatsApp preview plus a real tap looks like — must not
    // approve twice.
    let spent = sqlx::query(
        r#"UPDATE campus_ops.guardian_approval_tokens
           SET used_at = now(), decision = $3
           WHERE tenant_id = $1 AND id = $2 AND used_at IS NULL"#,
    )
    .bind(token_row.tenant)
    .bind(token_row.id)
    .bind(&input.decision)
    .execute(&mut *tx)
    .await?;
    if spent.rows_affected() == 0 {
        return Err(ApiError::Conflict("This link has already been used".into()));
    }

    let outcome = advance_gatepass_step(
        &mut tx,
        token_row.tenant,
        token_row.request_id,
        &input.decision,
        input.note.as_deref(),
        // The audit trail names the phone that answered, since there is no
        // account behind this decision.
        &format!("guardian:{}", token_row.guardian_phone),
        Some("parent"),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(ApiResponse::new(json!({
        "state": outcome.next_state,
        "decision": input.decision,
        "guardianName": token_row.guardian_name,
    }))))
}

struct TokenRow {
    id: Uuid,
    tenant: Uuid,
    request_id: Uuid,
    guardian_name: String,
    guardian_phone: String,
}

/// Finds the tenant a link belongs to, and the request behind it.
///
/// A public route has no tenant header to trust, so the token is looked up in
/// each registered tenant until it matches. The hash is unique across the
/// column, so at most one can answer.
async fn resolve(state: &AppState, token: &str) -> ApiResult<(String, TokenRow, Value)> {
    // A token is two uuids of hex; anything else is not worth a database round
    // trip, let alone one per tenant.
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::NotFound("This link is not valid".into()));
    }
    let hash = token_hash(token);

    for slug in state.registered_tenant_slugs().await? {
        let db = state.tenant_database(&slug).await?;
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                Uuid,
                Uuid,
                String,
                String,
                Option<DateTime<Utc>>,
                DateTime<Utc>,
                String,
                String,
                String,
                DateTime<Utc>,
                String,
            ),
        >(
            r#"SELECT token.id, token.tenant_id, token.request_id,
                      token.guardian_name, token.guardian_phone,
                      token.used_at, token.expires_at,
                      request.requester_name, request.destination, request.reason,
                      request.departure_at, request.state
               FROM campus_ops.guardian_approval_tokens token
               JOIN campus_ops.gatepass_requests request
                 ON request.tenant_id = token.tenant_id AND request.id = token.request_id
               WHERE token.token_hash = $1"#,
        )
        .bind(&hash)
        .fetch_optional(db.pool())
        .await?;

        let Some(row) = row else { continue };

        if row.5.is_some() {
            return Err(ApiError::Conflict("This link has already been used".into()));
        }
        if row.6 < Utc::now() {
            return Err(ApiError::Conflict("This link has expired".into()));
        }

        let value = json!({
            "studentName": row.7,
            "destination": row.8,
            "reason": row.9,
            "departureAt": row.10,
            "state": row.11,
            "guardianName": row.3,
            "expiresAt": row.6,
        });
        return Ok((
            slug,
            TokenRow {
                id: row.0,
                tenant: row.1,
                request_id: row.2,
                guardian_name: row.3,
                guardian_phone: row.4,
            },
            value,
        ));
    }

    Err(ApiError::NotFound("This link is not valid".into()))
}
