//! Visitor passes & invitations.
//!
//! A visitor is not a member of the institution — no account, no role, no
//! membership — so everything about them lives on the pass. Two kinds:
//!
//! * **parent** — raised by a student for their own guardian, silver card.
//! * **guest** — raised by an administrator for anyone else, gold card.
//!
//! When approved or when created directly by an administrator, a token is minted,
//! the pass card PNG is rendered, stored in tenant media, and delivered to the
//! visitor over WhatsApp with template `supercampus_visitor_invitation_qr_v1`.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    passes::{self, PassTier},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};
use supercampus_notifications::whatsapp::{DeliveryOutcome, WhatsAppMessage};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisitorPassInput {
    /// `parent` or `guest`.
    pub visitor_kind: String,
    pub visitor_name: String,
    pub visitor_phone: String,
    pub purpose: String,
    #[serde(default)]
    pub relationship: Option<String>,
    /// Whom the visitor is coming to see. Ignored for a parent pass, where the
    /// host is always the student raising it.
    #[serde(default)]
    pub host_user_id: Option<String>,
    #[serde(default)]
    pub host_name: Option<String>,
    pub visit_from: DateTime<Utc>,
    pub visit_until: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisitorDecisionInput {
    /// `approved` or `rejected`.
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Helper function to dispatch WhatsApp invitation with template.
async fn dispatch_visitor_whatsapp(
    state: &AppState,
    visitor_name: &str,
    visitor_phone: &str,
    host_name: &str,
    _kind: &str,
    visit_from: DateTime<Utc>,
    visit_until: DateTime<Utc>,
    image_url: &str,
    raw_token: &str,
) -> (String, Option<String>) {
    let date_str = visit_from.format("%d %b %Y").to_string();
    let time_str = format!("{} - {}", visit_from.format("%I:%M %p"), visit_until.format("%I:%M %p"));
    let body = format!(
        "Hello {visitor_name},\n\nYour visit to SuperCampus has been scheduled.\n\nDate: {date_str}\nTime: {time_str}\n\nPlease show the QR pass attached to this message at the campus gate.\n\nFooter:\nSuperCampus Visitor Access"
    );
    let template_name = std::env::var("GALLABOX_TEMPLATE_VISITOR_INVITATION")
        .or_else(|_| std::env::var("GALLABOX_TEMPLATE_VISITOR_PASS"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "supercampus_visitor_invitation_qr_v1".to_owned());
    let public_page_url = format!(
        "{}/api/v1/public/visitor/invitations/{raw_token}/page",
        crate::routes::public_base_url()
    );

    let outcome = state
        .whatsapp()
        .send(WhatsAppMessage {
            to: visitor_phone.to_owned(),
            body,
            media_url: Some(image_url.to_owned()),
            template_variables: vec![visitor_name.to_owned(), date_str.clone(), time_str.clone()],
            recipient_name: Some(visitor_name.to_owned()),
            template_name: Some(template_name),
            template_values: [
                ("VisitorName".to_owned(), visitor_name.to_owned()),
                ("HostName".to_owned(), host_name.to_owned()),
                ("VisitDate".to_owned(), date_str),
                ("VisitTime".to_owned(), time_str),
                ("PassUrl".to_owned(), image_url.to_owned()),
                ("PageUrl".to_owned(), public_page_url),
            ]
            .into_iter()
            .collect(),
            button_values: vec![serde_json::json!({
                "index": 0,
                "sub_type": "url",
                "parameters": {"type": "text", "text": raw_token.to_owned()}
            })],
        })
        .await;

    match outcome {
        Ok(DeliveryOutcome::Sent { .. }) => ("sent".to_owned(), None),
        Ok(DeliveryOutcome::NotConfigured) => ("not_configured".to_owned(), None),
        Err(error) => {
            tracing::error!(error = ?error, "visitor pass could not be delivered");
            ("failed".to_owned(), Some(error.to_string()))
        }
    }
}

/// Raises a visitor pass or invitation.
pub async fn create_visitor_pass(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<VisitorPassInput>,
) -> ApiResult<(StatusCode, Json<crate::models::ApiResponse<Value>>)> {
    crate::operations::require(&access, "gatepass.visitor.create")?;

    let kind = input.visitor_kind.trim();
    if !matches!(kind, "parent" | "guest") {
        return Err(ApiError::BadRequest(
            "A visitor is either a parent or a guest".into(),
        ));
    }

    let is_institutional = matches!(
        access.scope_for("gatepass.visitor.create").unwrap_or("own"),
        "institution" | "all"
    ) || matches!(
        access.scope_for("gatepass.visitor.approve").unwrap_or("own"),
        "institution" | "all"
    );

    if kind == "guest" && !is_institutional {
        return Err(ApiError::Forbidden);
    }

    let phone = normalise_phone(&input.visitor_phone);
    if phone.len() < 8 {
        return Err(ApiError::BadRequest(
            "That is not a phone number WhatsApp can reach".into(),
        ));
    }
    if input.visit_until <= input.visit_from {
        return Err(ApiError::BadRequest(
            "A visit has to end after it begins".into(),
        ));
    }
    if input.visitor_name.trim().is_empty() || input.purpose.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "A visitor needs a name and a reason for the visit".into(),
        ));
    }

    let relationship = input
        .relationship
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(if kind == "guest" { "Guest" } else { "Parent" });

    let (host_user_id, host_name) = if kind == "parent" {
        (principal.student.id.clone(), principal.student.name.clone())
    } else {
        (
            input
                .host_user_id
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| principal.student.id.clone()),
            input
                .host_name
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| principal.student.name.clone()),
        )
    };

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = crate::operations::tenant_id(db.pool(), &principal.student.tenant_id).await?;

    let auto_approve = is_institutional || kind == "guest";

    let (initial_state, raw_token, token_hash, pass_image_url, delivery_state, delivery_error) = if auto_approve {
        let raw_token = Uuid::new_v4().to_string();
        let token_hash = crate::operations::token_hash(&raw_token);
        let tier = PassTier::for_visitor_kind(kind);

        let png = passes::render(&raw_token, tier, 740).map_err(|error| {
            tracing::error!(error = ?error, "failed to render a visitor pass card");
            ApiError::Internal
        })?;
        let temp_id = Uuid::new_v4();
        let stored = crate::media::store_rendered_png(
            &principal.student.tenant_id,
            &format!("visitor-pass-{temp_id}.png"),
            png,
        )
        .await?;
        let image_url = stored
            .get("secureUrl")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        let (ds, de) = dispatch_visitor_whatsapp(
            &state,
            input.visitor_name.trim(),
            &phone,
            &host_name,
            kind,
            input.visit_from,
            input.visit_until,
            &image_url,
            &raw_token,
        )
        .await;

        ("approved", Some(raw_token), Some(token_hash), Some(image_url), ds, de)
    } else {
        ("pending_admin", None, None, None, "pending".to_string(), None)
    };

    let mut tx = db.pool().begin().await?;

    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant.to_string())
        .execute(&mut *tx)
        .await?;

    let value = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO campus_ops.visitor_passes
               (tenant_id, visitor_kind, visitor_name, visitor_phone, purpose, relationship,
                host_user_id, host_name, requested_by, visit_from, visit_until,
                state, qr_token_hash, pass_image_url, delivery_state, delivery_error, delivered_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,CASE WHEN $15 = 'sent' THEN now() ELSE NULL END)
           RETURNING jsonb_build_object(
               'id', id, 'visitorKind', visitor_kind, 'visitorName', visitor_name,
               'visitorPhone', visitor_phone, 'purpose', purpose, 'relationship', relationship,
               'hostUserId', host_user_id, 'hostName', host_name,
               'visitFrom', visit_from, 'visitUntil', visit_until,
               'state', state, 'passImageUrl', pass_image_url, 'deliveryState', delivery_state,
               'deliveryError', delivery_error, 'createdAt', created_at)"#,
    )
    .bind(tenant)
    .bind(kind)
    .bind(input.visitor_name.trim())
    .bind(&phone)
    .bind(input.purpose.trim())
    .bind(relationship)
    .bind(&host_user_id)
    .bind(&host_name)
    .bind(&principal.student.id)
    .bind(input.visit_from)
    .bind(input.visit_until)
    .bind(initial_state)
    .bind(token_hash.as_deref())
    .bind(pass_image_url.as_deref())
    .bind(&delivery_state)
    .bind(delivery_error.as_deref())
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let mut result_json = value;
    if let (Some(obj), Some(token_str)) = (result_json.as_object_mut(), raw_token) {
        obj.insert("rawToken".into(), json!(token_str));
    }

    Ok((
        StatusCode::CREATED,
        Json(crate::models::ApiResponse::new(result_json)),
    ))
}

/// Lists visitor passes.
pub async fn list_visitor_passes(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<crate::models::ApiResponse<Value>>> {
    crate::operations::require_any(
        &access,
        &["gatepass.visitor.read", "gatepass.visitor.create"],
    )?;
    let manage = matches!(
        access
            .scope_for("gatepass.visitor.read")
            .or_else(|| access.scope_for("gatepass.visitor.create"))
            .unwrap_or("own"),
        "institution" | "all"
    );

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = crate::operations::tenant_id(db.pool(), &principal.student.tenant_id).await?;

    let mut tx = db.pool().begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant.to_string())
        .execute(&mut *tx)
        .await?;

    let value = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
               'id', id, 'visitorKind', visitor_kind, 'visitorName', visitor_name,
               'visitorPhone', visitor_phone, 'purpose', purpose, 'relationship', relationship,
               'hostUserId', host_user_id, 'hostName', host_name,
               'visitFrom', visit_from, 'visitUntil', visit_until,
               'state', state, 'deliveryState', delivery_state,
               'deliveryError', delivery_error, 'passImageUrl', pass_image_url,
               'checkedInAt', checked_in_at, 'checkedOutAt', checked_out_at,
               'tier', CASE WHEN visitor_kind = 'guest' THEN 'gold' ELSE 'silver' END,
               'createdAt', created_at, 'updatedAt', updated_at
           ) ORDER BY created_at DESC), '[]'::jsonb)
           FROM campus_ops.visitor_passes
           WHERE tenant_id = $1
             AND ($3 OR host_user_id = $2 OR requested_by = $2)"#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .bind(manage)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(crate::models::ApiResponse::new(json!({
        "visitors": value,
        "canManage": manage,
    }))))
}

/// Approves or rejects a visitor pass.
pub async fn decide_visitor_pass(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(pass_id): Path<Uuid>,
    Json(input): Json<VisitorDecisionInput>,
) -> ApiResult<Json<crate::models::ApiResponse<Value>>> {
    crate::operations::require(&access, "gatepass.visitor.approve")?;
    if !matches!(input.decision.as_str(), "approved" | "rejected") {
        return Err(ApiError::BadRequest(
            "A decision is either approved or rejected".into(),
        ));
    }

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = crate::operations::tenant_id(db.pool(), &principal.student.tenant_id).await?;

    // Set tenant context for RLS. Using false (session-level) so the setting
    // persists across multiple queries on this pooled connection.
    sqlx::query("SELECT set_config('app.tenant_id', $1, false)")
        .bind(tenant.to_string())
        .execute(db.pool())
        .await?;

    let pending = sqlx::query_as::<_, (String, String, String, String, DateTime<Utc>, DateTime<Utc>)>(
        r#"SELECT visitor_kind, visitor_name, visitor_phone, host_name, visit_from, visit_until
           FROM campus_ops.visitor_passes
           WHERE tenant_id = $1 AND id = $2 AND state = 'pending_admin'"#,
    )
    .bind(tenant)
    .bind(pass_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::Conflict("This pass is not awaiting a decision".into()))?;

    if input.decision == "rejected" {
        let value = sqlx::query_scalar::<_, Value>(
            r#"UPDATE campus_ops.visitor_passes
               SET state = 'rejected', decided_by = $3, decision_note = $4, updated_at = now()
               WHERE tenant_id = $1 AND id = $2
               RETURNING jsonb_build_object('id', id, 'state', state, 'updatedAt', updated_at)"#,
        )
        .bind(tenant)
        .bind(pass_id)
        .bind(&principal.student.id)
        .bind(input.note.as_deref())
        .fetch_one(db.pool())
        .await?;
        return Ok(Json(crate::models::ApiResponse::new(value)));
    }

    let (kind, visitor_name, visitor_phone, host_name, visit_from, visit_until) = pending;
    let tier = PassTier::for_visitor_kind(&kind);

    let raw_token = Uuid::new_v4().to_string();
    let token_hash = crate::operations::token_hash(&raw_token);

    let png = passes::render(&raw_token, tier, 740).map_err(|error| {
        tracing::error!(error = ?error, "failed to render a visitor pass card");
        ApiError::Internal
    })?;
    let stored = crate::media::store_rendered_png(
        &principal.student.tenant_id,
        &format!("visitor-pass-{pass_id}.png"),
        png,
    )
    .await?;
    let image_url = stored
        .get("secureUrl")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    sqlx::query(
        r#"UPDATE campus_ops.visitor_passes
           SET state = 'approved', qr_token_hash = $3, pass_image_url = $4,
               decided_by = $5, decision_note = $6, updated_at = now()
           WHERE tenant_id = $1 AND id = $2"#,
    )
    .bind(tenant)
    .bind(pass_id)
    .bind(&token_hash)
    .bind(&image_url)
    .bind(&principal.student.id)
    .bind(input.note.as_deref())
    .execute(db.pool())
    .await?;

    let (delivery_state, delivery_error) = dispatch_visitor_whatsapp(
        &state,
        &visitor_name,
        &visitor_phone,
        &host_name,
        &kind,
        visit_from,
        visit_until,
        &image_url,
        &raw_token,
    )
    .await;

    let value = sqlx::query_scalar::<_, Value>(
        r#"UPDATE campus_ops.visitor_passes
           SET delivery_state = $3, delivery_error = $4,
               delivered_at = CASE WHEN $3 = 'sent' THEN now() ELSE NULL END,
               updated_at = now()
           WHERE tenant_id = $1 AND id = $2
           RETURNING jsonb_build_object(
               'id', id, 'state', state, 'tier', CASE WHEN visitor_kind = 'guest'
                   THEN 'gold' ELSE 'silver' END,
               'passImageUrl', pass_image_url, 'deliveryState', delivery_state,
               'deliveryError', delivery_error, 'updatedAt', updated_at)"#,
    )
    .bind(tenant)
    .bind(pass_id)
    .bind(&delivery_state)
    .bind(delivery_error.as_deref())
    .fetch_one(db.pool())
    .await?;

    Ok(Json(crate::models::ApiResponse::new(value)))
}

/// Cancels a visitor pass.
pub async fn cancel_visitor_pass(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(pass_id): Path<Uuid>,
) -> ApiResult<Json<crate::models::ApiResponse<Value>>> {
    crate::operations::require_any(
        &access,
        &["gatepass.visitor.create", "gatepass.visitor.approve"],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = crate::operations::tenant_id(db.pool(), &principal.student.tenant_id).await?;

    let mut tx = db.pool().begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant.to_string())
        .execute(&mut *tx)
        .await?;

    let value = sqlx::query_scalar::<_, Value>(
        r#"UPDATE campus_ops.visitor_passes
           SET state = 'cancelled', updated_at = now()
           WHERE tenant_id = $1 AND id = $2 AND state NOT IN ('checked_in', 'checked_out')
           RETURNING jsonb_build_object('id', id, 'state', state, 'updatedAt', updated_at)"#,
    )
    .bind(tenant)
    .bind(pass_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Conflict("This visitor pass cannot be cancelled".into()))?;

    tx.commit().await?;

    Ok(Json(crate::models::ApiResponse::new(value)))
}

/// Public verification endpoint for visitor passes.
pub async fn get_public_visitor_pass(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<crate::models::ApiResponse<Value>>> {
    let token = token.trim();
    if token.is_empty() {
        return Err(ApiError::BadRequest("Token is required".into()));
    }
    let hash = crate::operations::token_hash(token);
    let database = state.database().ok_or_else(|| ApiError::ServiceUnavailable("Storage unavailable".into()))?;

    let row = sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>, String, Option<String>, Option<DateTime<Utc>>)>(
        r#"SELECT visitor_name, host_name, purpose, visit_from, visit_until, state, pass_image_url, checked_in_at
           FROM campus_ops.visitor_passes
           WHERE qr_token_hash = $1"#,
    )
    .bind(hash)
    .fetch_optional(database.pool())
    .await?;

    let Some((visitor_name, host_name, purpose, visit_from, visit_until, state_str, pass_image_url, checked_in_at)) = row else {
        return Ok(Json(crate::models::ApiResponse::new(json!({
            "valid": false,
            "reason": "Invalid or unknown visitor pass token"
        }))));
    };

    let now = Utc::now();
    let is_valid = matches!(state_str.as_str(), "approved" | "sent" | "active")
        && now >= (visit_from - chrono::Duration::minutes(30))
        && now <= visit_until;

    Ok(Json(crate::models::ApiResponse::new(json!({
        "valid": is_valid,
        "visitorName": visitor_name,
        "hostName": host_name,
        "purpose": purpose,
        "visitFrom": visit_from,
        "visitUntil": visit_until,
        "state": state_str,
        "passImageUrl": pass_image_url,
        "checkedInAt": checked_in_at
    }))))
}

/// Renders an HTML page or JSON representation for visitor invitation links on WhatsApp.
pub async fn show_public_visitor_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let token = token.trim();
    let hash = crate::operations::token_hash(token);
    let database = state.database().ok_or_else(|| ApiError::ServiceUnavailable("Storage unavailable".into()))?;

    let row = sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>, String, Option<String>)>(
        r#"SELECT visitor_name, host_name, purpose, visit_from, visit_until, state, pass_image_url
           FROM campus_ops.visitor_passes
           WHERE qr_token_hash = $1"#,
    )
    .bind(hash)
    .fetch_optional(database.pool())
    .await?;

    let Some((visitor_name, host_name, purpose, visit_from, visit_until, state_str, pass_image_url)) = row else {
        return Ok(axum::response::Html("<h1>Invalid Visitor Pass</h1><p>The visitor pass link is invalid or expired.</p>".to_owned()).into_response());
    };

    let pass_img = pass_image_url.unwrap_or_default();
    let date_str = visit_from.format("%A, %d %B %Y").to_string();
    let time_str = format!("{} - {}", visit_from.format("%I:%M %p"), visit_until.format("%I:%M %p"));

    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Visitor Access Pass - SuperCampus</title>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: #0f172a; color: #f8fafc; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; padding: 1rem; }}
        .card {{ background: #1e293b; border: 1px solid #334155; border-radius: 16px; max-width: 420px; width: 100%; overflow: hidden; box-shadow: 0 20px 25px -5px rgba(0,0,0,0.5); }}
        .header {{ background: linear-gradient(135deg, #4f46e5, #06b6d4); padding: 1.5rem; text-align: center; }}
        .header h1 {{ margin: 0; font-size: 1.25rem; color: #fff; letter-spacing: 0.05em; }}
        .content {{ padding: 1.5rem; text-align: center; }}
        .qr-img {{ width: 100%; max-width: 320px; border-radius: 12px; margin-bottom: 1.5rem; border: 2px solid #334155; }}
        .info {{ text-align: left; background: #0f172a; padding: 1rem; border-radius: 8px; font-size: 0.9rem; line-height: 1.6; color: #94a3b8; }}
        .info strong {{ color: #f8fafc; }}
        .status {{ display: inline-block; padding: 0.25rem 0.75rem; border-radius: 9999px; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; margin-top: 1rem; background: #10b981; color: #064e3b; }}
    </style>
</head>
<body>
    <div class="card">
        <div class="header">
            <h1>SUPERCAMPUS VISITOR PASS</h1>
        </div>
        <div class="content">
            <img src="{pass_img}" alt="Visitor QR Pass" class="qr-img" />
            <div class="info">
                <div><strong>Visitor:</strong> {visitor_name}</div>
                <div><strong>Host:</strong> {host_name}</div>
                <div><strong>Purpose:</strong> {purpose}</div>
                <div><strong>Date:</strong> {date_str}</div>
                <div><strong>Time:</strong> {time_str}</div>
            </div>
            <div class="status">{state_str}</div>
        </div>
    </div>
</body>
</html>"#);

    Ok(axum::response::Html(html).into_response())
}

/// Digits with a leading `+`, which is the only form Twilio accepts.
fn normalise_phone(number: &str) -> String {
    let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        String::new()
    } else {
        format!("+{digits}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_numbers_reach_e164() {
        assert_eq!(normalise_phone("+91 98765 43210"), "+919876543210");
        assert_eq!(normalise_phone("098765-43210"), "+09876543210");
        assert_eq!(normalise_phone("not a number"), "");
    }

    #[test]
    fn the_tier_follows_the_visitor_kind() {
        assert_eq!(PassTier::for_visitor_kind("guest").as_str(), "gold");
        assert_eq!(PassTier::for_visitor_kind("parent").as_str(), "silver");
    }
}
