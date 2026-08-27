//! Visitor passes.
//!
//! A visitor is not a member of the institution — no account, no role, no
//! membership — so everything about them lives on the pass. Two kinds, which
//! differ in who may raise one and in nothing else:
//!
//! * **parent** — raised by a student for their own guardian, silver card.
//! * **guest** — raised by an administrator for anyone else, gold card.
//!
//! Both wait on an administrator. On approval a token is minted, the card is
//! rendered, stored in the tenant's media folder, and sent to the visitor over
//! WhatsApp, because a visitor has no app to open.
//!
//! Approval and delivery are recorded separately on purpose: a pass can be
//! validly approved and still not have reached a phone, and whoever is standing
//! at the gate needs to be able to tell those apart.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
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

/// Raises a visitor pass.
///
/// A student may raise one only for their own guardian, and only for
/// themselves as host — the host is taken from the session rather than the body
/// so a student cannot invite someone to another student.
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

    // Raising a guest pass is an institutional act, so it takes institutional
    // reach. A student holds `visitor.create` for their own guardian and stops
    // there.
    if kind == "guest" {
        let scope = access.scope_for("gatepass.visitor.create").unwrap_or("own");
        if !matches!(scope, "institution" | "all") {
            return Err(ApiError::Forbidden);
        }
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

    let value = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO campus_ops.visitor_passes
               (tenant_id, visitor_kind, visitor_name, visitor_phone, purpose,
                host_user_id, host_name, requested_by, visit_from, visit_until)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           RETURNING jsonb_build_object(
               'id', id, 'visitorKind', visitor_kind, 'visitorName', visitor_name,
               'visitorPhone', visitor_phone, 'purpose', purpose,
               'hostUserId', host_user_id, 'hostName', host_name,
               'visitFrom', visit_from, 'visitUntil', visit_until,
               'state', state, 'deliveryState', delivery_state, 'createdAt', created_at)"#,
    )
    .bind(tenant)
    .bind(kind)
    .bind(input.visitor_name.trim())
    .bind(&phone)
    .bind(input.purpose.trim())
    .bind(&host_user_id)
    .bind(&host_name)
    .bind(&principal.student.id)
    .bind(input.visit_from)
    .bind(input.visit_until)
    .fetch_one(db.pool())
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(crate::models::ApiResponse::new(value)),
    ))
}

/// Lists visitor passes.
///
/// Institutional reach sees the tenant's; anyone else sees the ones they raised
/// or are hosting, which is what a student needs to check whether their
/// parent's pass has been approved.
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

    let value = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
               'id', id, 'visitorKind', visitor_kind, 'visitorName', visitor_name,
               'visitorPhone', visitor_phone, 'purpose', purpose,
               'hostUserId', host_user_id, 'hostName', host_name,
               'visitFrom', visit_from, 'visitUntil', visit_until,
               'state', state, 'deliveryState', delivery_state,
               'deliveryError', delivery_error, 'passImageUrl', pass_image_url,
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
    .fetch_one(db.pool())
    .await?;

    Ok(Json(crate::models::ApiResponse::new(json!({
        "visitors": value,
        "canManage": manage,
    }))))
}

/// Approves or rejects a visitor pass.
///
/// On approval this mints the token, renders the card, stores it and sends it.
/// The send is deliberately not fatal: the pass is validly approved either way,
/// and failing the whole request would leave an administrator unable to tell
/// whether the approval had been recorded. The failure is written onto the pass
/// instead, where it can be seen and retried.
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

    let pending = sqlx::query_as::<_, (String, String, String, String)>(
        r#"SELECT visitor_kind, visitor_name, visitor_phone, host_name
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

    let (kind, visitor_name, visitor_phone, host_name) = pending;
    let tier = PassTier::for_visitor_kind(&kind);

    // The QR carries an opaque token and nothing else, so a photograph of the
    // card over someone's shoulder discloses nothing about the visit.
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

    // Approve first. Whatever WhatsApp does next, the decision is recorded.
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

    let body = format!(
        "{visitor_name}, your {} campus pass is ready.\nVisiting: {host_name}\nShow this QR at the gate.",
        if kind == "guest" { "guest" } else { "visitor" }
    );
    let outcome = state
        .whatsapp()
        .send(WhatsAppMessage {
            to: visitor_phone.clone(),
            body,
            media_url: Some(image_url.clone()),
            // Used only when the tenant has an approved template configured.
            // The card's link rides along as a variable, because a template
            // cannot carry freeform text and a trial account cannot carry an
            // attachment at all.
            template_variables: vec![visitor_name.clone(), image_url.clone()],
        })
        .await;

    let (delivery_state, delivery_error) = match outcome {
        Ok(DeliveryOutcome::Sent { .. }) => ("sent", None),
        Ok(DeliveryOutcome::NotConfigured) => ("not_configured", None),
        Err(error) => {
            tracing::error!(error = ?error, %pass_id, "visitor pass could not be delivered");
            ("failed", Some(error.to_string()))
        }
    };

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
    .bind(delivery_state)
    .bind(delivery_error.as_deref())
    .fetch_one(db.pool())
    .await?;

    Ok(Json(crate::models::ApiResponse::new(value)))
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
