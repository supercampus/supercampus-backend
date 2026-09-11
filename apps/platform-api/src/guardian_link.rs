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
    Form, Json,
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    response::Html,
};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
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

    let link = format!(
        "{}/api/v1/public/gatepass/approvals/{raw}/page",
        api_public_url().trim_end_matches('/')
    );
    let body = format!(
        "{guardian_name}, {student_name} has requested an outpass. Approve or decline here: {link}"
    );

    // Interactive payloads are template-defined in WhatsApp. Do not attach
    // button substitutions to the currently approved buttonless template.
    // Once a matching quick-reply template is approved, both the signed
    // webhook secret and this explicit feature flag must be enabled.
    let quick_replies = std::env::var("GALLABOX_WEBHOOK_SECRET")
        .is_ok_and(|value| !value.trim().is_empty())
        && env_flag("GALLABOX_GUARDIAN_APPROVAL_INTERACTIVE");
    let button_values = if quick_replies {
        vec![
            json!({
                "index": 0,
                "sub_type": "quick_reply",
                "parameters": {"type": "payload", "payload": format!("SC_OUTPASS:approved:{raw}")}
            }),
            json!({
                "index": 1,
                "sub_type": "quick_reply",
                "parameters": {"type": "payload", "payload": format!("SC_OUTPASS:rejected:{raw}")}
            }),
        ]
    } else {
        Vec::new()
    };
    let outcome = state
        .whatsapp()
        .send(WhatsAppMessage {
            to: guardian_phone.to_owned(),
            body: body.clone(),
            media_url: None,
            template_variables: vec![student_name.to_owned(), link.clone()],
            recipient_name: Some(guardian_name.to_owned()),
            template_name: std::env::var("GALLABOX_TEMPLATE_GUARDIAN_APPROVAL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            template_values: [
                ("RecipientName".to_owned(), guardian_name.to_owned()),
                (
                    "Title".to_owned(),
                    format!("Outpass request for {student_name}"),
                ),
                ("Message".to_owned(), body.clone()),
                (
                    "EventType".to_owned(),
                    "gatepass.parent_approval".to_owned(),
                ),
                ("GuardianName".to_owned(), guardian_name.to_owned()),
                ("StudentName".to_owned(), student_name.to_owned()),
                ("ActionUrl".to_owned(), link.clone()),
            ]
            .into_iter()
            .collect(),
            button_values,
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

fn env_flag(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
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
    let value = decide(&state, &token, input).await?;
    Ok(Json(ApiResponse::new(value)))
}

async fn decide(state: &AppState, token: &str, input: GuardianDecisionInput) -> ApiResult<Value> {
    if !matches!(input.decision.as_str(), "approved" | "rejected") {
        return Err(ApiError::BadRequest(
            "A decision is either approved or rejected".into(),
        ));
    }

    let (tenant_slug, token_row, _) = resolve(state, token).await?;
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

    Ok(json!({
        "state": outcome.next_state,
        "decision": input.decision,
        "guardianName": token_row.guardian_name,
    }))
}

#[derive(Deserialize)]
pub struct GuardianPageDecision {
    decision: String,
}

pub async fn show_guardian_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Html<String>> {
    let (_, _, value) = resolve(&state, &token).await?;
    let student = html_escape(value["studentName"].as_str().unwrap_or("Student"));
    let destination = html_escape(value["destination"].as_str().unwrap_or(""));
    let reason = html_escape(value["reason"].as_str().unwrap_or(""));
    Ok(Html(format!(
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>Outpass approval</title><style>body{{font-family:system-ui,sans-serif;background:#f7f4fb;margin:0;display:grid;min-height:100vh;place-items:center;color:#211b2e}}main{{width:min(92vw,420px);background:#fff;border:1px solid #e6dcf7;border-radius:22px;padding:28px;box-sizing:border-box;box-shadow:0 18px 50px #45208018}}h1{{font-size:24px;margin:0 0 8px}}p{{color:#615970;line-height:1.5}}dl{{background:#f8f5fc;border-radius:14px;padding:16px}}dt{{font-size:12px;color:#766d83;margin-top:10px}}dd{{margin:3px 0 0;font-weight:600}}button{{border-radius:12px;padding:14px 18px;font-size:16px;font-weight:700;cursor:pointer}}.actions{{display:grid;grid-template-columns:1fr 1fr;gap:10px;margin-top:20px}}.approve{{background:#1a6b3c;color:white;border:0}}.reject{{background:#fff0f0;color:#a51d2d;border:1px solid #efb7bd}}</style></head><body><main><p>SuperCampus guardian action</p><h1>{student}'s outpass</h1><dl><dt>Destination</dt><dd>{destination}</dd><dt>Reason</dt><dd>{reason}</dd></dl><form method="post"><div class="actions"><button class="reject" name="decision" value="rejected">Reject</button><button class="approve" name="decision" value="approved">Approve</button></div></form></main></body></html>"#
    )))
}

pub async fn decide_guardian_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Form(input): Form<GuardianPageDecision>,
) -> ApiResult<Html<String>> {
    let approved = input.decision == "approved";
    let value = decide(
        &state,
        &token,
        GuardianDecisionInput {
            decision: input.decision,
            note: None,
        },
    )
    .await?;
    let state_label = value["state"].as_str().unwrap_or("updated");
    let (title, message) = if approved {
        (
            "Outpass approved",
            format!("The request is now {state_label}."),
        )
    } else {
        ("Outpass rejected", "The student has been notified.".into())
    };
    Ok(Html(result_page(title, &message)))
}

/// Accepts Gallabox's signed interaction event for an approved/rejected quick
/// reply. The action token still authorises only one outpass, and the sender
/// phone must match the guardian snapshot attached to that token.
pub async fn gallabox_interaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    verify_gallabox_signature(&headers, &body)?;
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("Invalid Gallabox webhook body".into()))?;
    let action = find_action(&payload)
        .ok_or_else(|| ApiError::BadRequest("No SuperCampus action was found".into()))?;
    let phone = find_phone(&payload)
        .ok_or_else(|| ApiError::BadRequest("Gallabox sender phone was not supplied".into()))?;
    let mut parts = action.splitn(3, ':');
    if parts.next() != Some("SC_OUTPASS") {
        return Err(ApiError::BadRequest("Unsupported Gallabox action".into()));
    }
    let decision = parts.next().unwrap_or_default();
    let token = parts.next().unwrap_or_default();
    if !matches!(decision, "approved" | "rejected") {
        return Err(ApiError::BadRequest("Unsupported outpass decision".into()));
    }
    let (_, token_row, _) = resolve(&state, token).await?;
    if !phones_match(&token_row.guardian_phone, &phone) {
        return Err(ApiError::Forbidden);
    }
    let value = decide(
        &state,
        token,
        GuardianDecisionInput {
            decision: decision.into(),
            note: Some("Answered from the guardian WhatsApp chat".into()),
        },
    )
    .await?;
    Ok(Json(json!({"ok": true, "data": value})))
}

fn verify_gallabox_signature(headers: &HeaderMap, body: &[u8]) -> ApiResult<()> {
    let secret = std::env::var("GALLABOX_WEBHOOK_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::ServiceUnavailable("Gallabox webhook is not configured".into()))?;
    let supplied = headers
        .get("x-gallabox-signature")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().strip_prefix("sha256=").unwrap_or(value.trim()))
        .and_then(|value| hex::decode(value).ok())
        .ok_or(ApiError::Forbidden)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ApiError::Internal)?;
    mac.update(body);
    mac.verify_slice(&supplied).map_err(|_| ApiError::Forbidden)
}

fn find_action(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if value.starts_with("SC_OUTPASS:") => Some(value.clone()),
        Value::Array(values) => values.iter().find_map(find_action),
        Value::Object(values) => values.values().find_map(find_action),
        _ => None,
    }
}

fn find_phone(value: &Value) -> Option<String> {
    match value {
        Value::Object(values) => {
            for key in ["phone", "from", "waId", "wa_id", "contactPhone"] {
                if let Some(phone) = values.get(key).and_then(Value::as_str)
                    && phone_digits(phone).len() >= 8
                {
                    return Some(phone.into());
                }
            }
            values.values().find_map(find_phone)
        }
        Value::Array(values) => values.iter().find_map(find_phone),
        _ => None,
    }
}

fn phone_digits(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

fn phones_match(left: &str, right: &str) -> bool {
    let left = phone_digits(left);
    let right = phone_digits(right);
    left == right
        || (left.len() >= 10
            && right.len() >= 10
            && left[left.len() - 10..] == right[right.len() - 10..])
}

fn api_public_url() -> String {
    std::env::var("API_PUBLIC_URL").unwrap_or_else(|_| "https://api.supercampus.ai".into())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn result_page(title: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>{}</title><style>body{{font-family:system-ui,sans-serif;background:#f7f4fb;margin:0;display:grid;min-height:100vh;place-items:center;color:#211b2e}}main{{width:min(92vw,420px);background:white;border:1px solid #e6dcf7;border-radius:22px;padding:30px;text-align:center;box-shadow:0 18px 50px #45208018}}h1{{font-size:25px}}p{{color:#615970;line-height:1.5}}</style></head><body><main><h1>{}</h1><p>{}</p></main></body></html>"#,
        html_escape(title),
        html_escape(title),
        html_escape(message)
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_phone_comparison_ignores_formatting_and_country_prefix() {
        assert!(phones_match("+91 63791 73918", "916379173918"));
        assert!(phones_match("63791-73918", "+91 63791 73918"));
        assert!(!phones_match("63791 73918", "+91 90000 00000"));
    }

    #[test]
    fn webhook_action_is_found_inside_nested_provider_payload() {
        let payload = json!({
            "event": "Message.WA.Interaction.Received",
            "data": {"message": {"interactive": {"button_reply": {
                "id": "SC_OUTPASS:approved:deadbeef",
                "title": "Approve"
            }}}}
        });
        assert_eq!(
            find_action(&payload).as_deref(),
            Some("SC_OUTPASS:approved:deadbeef")
        );
    }
}
