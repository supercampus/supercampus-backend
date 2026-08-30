//! Persistent, tenant-scoped notification creation.
//!
//! Every push begins as an inbox row. Provider delivery is asynchronous, so a
//! failed phone transport can never roll back the business action that caused
//! the notification.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{error::ApiResult, state::AppState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    User(String),
    Role(String),
}

#[derive(Debug, Clone)]
pub struct NotificationSpec {
    pub recipient: Recipient,
    pub category: String,
    pub event_type: String,
    pub title: String,
    pub body: String,
    pub data: Value,
    pub priority: String,
    pub requires_action: bool,
    pub deep_link: Option<String>,
    pub deduplication_key: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn enqueue_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec: NotificationSpec,
) -> ApiResult<()> {
    let (recipient_user_id, recipient_role) = match spec.recipient {
        Recipient::User(user_id) => (Some(user_id), None),
        Recipient::Role(role) => (None, Some(role)),
    };
    sqlx::query(
        r#"INSERT INTO campus_ops.notifications
             (tenant_id,recipient_user_id,recipient_role,category,event_type,
              title,body,data,priority,requires_action,deep_link,
              deduplication_key,expires_at)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
           ON CONFLICT(tenant_id,deduplication_key)
             WHERE deduplication_key IS NOT NULL DO NOTHING"#,
    )
    .bind(tenant_id)
    .bind(recipient_user_id)
    .bind(recipient_role)
    .bind(spec.category)
    .bind(spec.event_type)
    .bind(spec.title)
    .bind(spec.body)
    .bind(spec.data)
    .bind(spec.priority)
    .bind(spec.requires_action)
    .bind(spec.deep_link)
    .bind(spec.deduplication_key)
    .bind(spec.expires_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Enqueues one notification outside a larger business transaction. Callers
/// use this only after their own mutation has completed; delivery remains
/// best-effort and cannot invalidate the completed action.
pub async fn enqueue(state: &AppState, tenant_slug: &str, spec: NotificationSpec) -> ApiResult<()> {
    let database = state.tenant_database(tenant_slug).await?;
    let mut tx = database.pool().begin().await?;
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(tenant_slug)
        .fetch_one(&mut *tx)
        .await?;
    enqueue_tx(&mut tx, tenant_id, spec).await?;
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn enqueue_record_change(
    state: &AppState,
    tenant_slug: &str,
    module_key: &str,
    record_id: Uuid,
    record_type: &str,
    data: &Value,
    action: &str,
) -> ApiResult<()> {
    let specs = record_notifications(module_key, record_id, record_type, data, action);
    if specs.is_empty() {
        return Ok(());
    }
    let database = state.tenant_database(tenant_slug).await?;
    let mut tx = database.pool().begin().await?;
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(tenant_slug)
        .fetch_one(&mut *tx)
        .await?;
    for spec in specs {
        enqueue_tx(&mut tx, tenant_id, spec).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn record_notifications(
    module_key: &str,
    record_id: Uuid,
    record_type: &str,
    data: &Value,
    action: &str,
) -> Vec<NotificationSpec> {
    let normalized_module = module_key.trim().to_ascii_lowercase();
    let normalized_type = record_type.trim().to_ascii_lowercase();
    let event_type = format!(
        "{}.{}.{}",
        normalized_module,
        normalized_type.replace('_', "."),
        action
    );
    let target_user = first_string(
        data,
        &[
            "recipientUserId",
            "studentUserId",
            "userId",
            "studentId",
            "memberUserId",
            "requesterUserId",
        ],
    );
    let payload = json!({
        "recordId": record_id,
        "recordType": record_type,
        "action": action,
        "record": data,
    });

    let details: Option<(&str, &str, String, &str, bool, &str)> =
        match (normalized_module.as_str(), normalized_type.as_str()) {
            ("fees", "fee_assignment" | "student_fee_accounts") => Some((
                "fees",
                "Fee account updated",
                "A new tuition-fee amount or due date is available.".into(),
                "high",
                true,
                "/tuition-fee",
            )),
            ("fees", "payments") => Some((
                "fees",
                "Fee payment updated",
                "Your tuition-fee payment status has been updated.".into(),
                "high",
                false,
                "/tuition-fee/payments",
            )),
            ("fees", "fines_penalties") => Some((
                "fees",
                "Fine or penalty updated",
                "Review the latest charge on your fee account.".into(),
                "high",
                true,
                "/tuition-fee",
            )),
            ("fees", "refunds") => Some((
                "fees",
                "Fee refund updated",
                "Your fee-refund request has a new status.".into(),
                "normal",
                false,
                "/tuition-fee/payments",
            )),
            ("fees", "concessions_scholarships") => Some((
                "fees",
                "Scholarship or concession updated",
                "Review the latest decision on your fee account.".into(),
                "normal",
                false,
                "/tuition-fee",
            )),
            ("fees", "fee_notifications") => Some((
                "fees",
                "Tuition-fee notice",
                first_string(data, &["message", "body", "description"])
                    .unwrap_or_else(|| "A new tuition-fee notice is available.".into()),
                "high",
                true,
                "/tuition-fee",
            )),
            ("examinations", "scheduling" | "schedule" | "exam_schedule") => Some((
                "examination",
                "Examination schedule updated",
                "Review the latest examination date, time and venue.".into(),
                "high",
                true,
                "/examinations/schedule",
            )),
            ("examinations", "grades" | "results" | "publishing" | "marks") => Some((
                "examination",
                "Examination result updated",
                "Your latest marks or results are available.".into(),
                "high",
                false,
                "/examinations/results",
            )),
            ("examinations", "eligibility") => Some((
                "examination",
                "Exam eligibility updated",
                "Review your latest examination eligibility status.".into(),
                "high",
                true,
                "/examinations/eligibility",
            )),
            ("examinations", "revaluation") => Some((
                "examination",
                "Revaluation request updated",
                "Your revaluation request has a new status.".into(),
                "normal",
                false,
                "/examinations/revaluation",
            )),
            ("academics", "marks" | "assessment" | "results") => Some((
                "academics",
                "Academic result updated",
                "New assessment marks or academic results are available.".into(),
                "normal",
                false,
                "/academics/results",
            )),
            ("library", "loans" | "fines" | "visit_pass") => Some((
                "library",
                "Library account updated",
                "Review the latest loan, fine or visit-pass update.".into(),
                "high",
                true,
                "/library",
            )),
            ("hostel", "leave" | "tickets" | "allocations") => Some((
                "hostel",
                "Hostel request updated",
                "Your hostel request or allocation has a new status.".into(),
                "high",
                false,
                "/hostel",
            )),
            ("transport", "routes" | "passes" | "alerts") => Some((
                "transport",
                "Transport update",
                "Review the latest route, pass or service update.".into(),
                "high",
                true,
                "/transport",
            )),
            _ => None,
        };
    let Some((category, title, body, priority, requires_action, deep_link)) = details else {
        return Vec::new();
    };

    let recipient = target_user.map(Recipient::User).or_else(|| {
        is_broadcast_record(&normalized_module, &normalized_type, data)
            .then(|| Recipient::Role("student".into()))
    });
    let Some(recipient) = recipient else {
        return Vec::new();
    };
    vec![NotificationSpec {
        recipient,
        category: category.into(),
        event_type,
        title: title.into(),
        body,
        data: payload,
        priority: priority.into(),
        requires_action,
        deep_link: Some(deep_link.into()),
        deduplication_key: Some(format!(
            "record:{module_key}:{record_id}:{action}:{}",
            status_value(data)
        )),
        expires_at: None,
    }]
}

fn is_broadcast_record(module: &str, record_type: &str, data: &Value) -> bool {
    let status = status_value(data);
    matches!(status.as_str(), "published" | "active" | "released")
        && matches!(
            (module, record_type),
            ("examinations", "scheduling" | "schedule" | "exam_schedule")
                | ("examinations", "grades" | "results" | "publishing")
                | ("academics", "results")
                | ("transport", "alerts")
        )
}

fn status_value(data: &Value) -> String {
    first_string(data, &["status", "state", "decision"])
        .unwrap_or_else(|| "unknown".into())
        .to_ascii_lowercase()
}

fn first_string(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        data.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_student_fee_assignment_is_direct_and_actionable() {
        let specs = record_notifications(
            "fees",
            Uuid::nil(),
            "fee_assignment",
            &json!({"studentUserId":"student-1","status":"issued"}),
            "created",
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].recipient, Recipient::User("student-1".into()));
        assert!(specs[0].requires_action);
        assert_eq!(specs[0].deep_link.as_deref(), Some("/tuition-fee"));
    }

    #[test]
    fn a_published_exam_schedule_targets_students() {
        let specs = record_notifications(
            "examinations",
            Uuid::nil(),
            "schedule",
            &json!({"status":"published"}),
            "updated",
        );
        assert_eq!(specs[0].recipient, Recipient::Role("student".into()));
        assert_eq!(specs[0].priority, "high");
    }

    #[test]
    fn internal_configuration_does_not_spam_users() {
        assert!(
            record_notifications(
                "fees",
                Uuid::nil(),
                "fee_heads",
                &json!({"status":"active"}),
                "created"
            )
            .is_empty()
        );
    }
}
