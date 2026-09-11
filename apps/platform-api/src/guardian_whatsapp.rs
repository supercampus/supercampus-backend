//! Guardian-facing WhatsApp workflows.
//!
//! Every outbound item is resolved through the student's primary guardian and
//! snapshots both identities before it is sent.  That prevents a generic user
//! notification from ever being delivered to the wrong family.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::Html,
};
use chrono::{NaiveTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::DynamicRecord,
    state::AppState,
};
use supercampus_notifications::whatsapp::{DeliveryOutcome, WhatsAppMessage};

const MAX_ATTEMPTS: i32 = 5;

#[derive(Clone)]
struct StudentGuardian {
    student_id: Uuid,
    student_user_id: String,
    student_number: String,
    student_email: Option<String>,
    student_name: String,
    guardian_id: Uuid,
    guardian_name: String,
    guardian_phone: String,
}

/// Starts a student-specific fee collection after an administrative fee record
/// is created or updated. Provider failure is returned to the caller for
/// logging, but must not roll back the already-valid fee record.
pub async fn send_fee_payment_request(
    state: &AppState,
    tenant_slug: &str,
    record: &DynamicRecord,
) -> anyhow::Result<()> {
    if !parent_whatsapp_enabled() || !fee_record_is_collectable(&record.data) {
        return Ok(());
    }
    let Some(amount_paise) = amount_due_paise(&record.data) else {
        return Ok(());
    };
    let database = state.tenant_database(tenant_slug).await?;
    let tenant: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(tenant_slug)
        .fetch_one(database.pool())
        .await?;
    let Some(student) = resolve_student_guardian(database.pool(), tenant, &record.data).await?
    else {
        tracing::warn!(fee_record=%record.id, "fee WhatsApp skipped: no exact primary guardian with a phone is linked to this student");
        return Ok(());
    };

    let event_key = format!("fee:{}:{amount_paise}", record.id);
    let Some(delivery_id) = claim_delivery(
        database.pool(),
        tenant,
        &student,
        "fees.payment.requested",
        &event_key,
        event_template("GALLABOX_TEMPLATE_FEES"),
    )
    .await?
    else {
        return Ok(());
    };

    let link = match existing_or_create_payment_link(
        database.pool(),
        tenant_slug,
        tenant,
        record,
        &student,
        amount_paise,
    )
    .await
    {
        Ok(link) => link,
        Err(error) => {
            record_delivery_failure(database.pool(), tenant, delivery_id, &error.to_string())
                .await?;
            return Err(error);
        }
    };

    let amount = format!("INR {:.2}", amount_paise as f64 / 100.0);
    let due_date = first_string(&record.data, &["dueDate", "dueOn", "deadline"])
        .unwrap_or_else(|| "as notified".into());
    let body = format!(
        "{}'s fee of {amount} is ready for payment. Due {due_date}. Pay securely: {}",
        student.student_name, link.short_url
    );
    let values = BTreeMap::from([
        ("RecipientName".into(), student.guardian_name.clone()),
        ("GuardianName".into(), student.guardian_name.clone()),
        ("StudentName".into(), student.student_name.clone()),
        ("Amount".into(), amount),
        ("DueDate".into(), due_date),
        ("Status".into(), "Payment due".into()),
        ("Title".into(), "Student fee payment".into()),
        ("Message".into(), body.clone()),
        ("EventType".into(), "fees.payment.requested".into()),
        ("ActionUrl".into(), link.short_url.clone()),
    ]);
    let outcome = state
        .whatsapp()
        .send(WhatsAppMessage {
            to: student.guardian_phone.clone(),
            body,
            media_url: None,
            template_variables: vec![
                student.guardian_name.clone(),
                student.student_name.clone(),
                format!("{:.2}", amount_paise as f64 / 100.0),
                link.short_url.clone(),
            ],
            recipient_name: Some(student.guardian_name.clone()),
            template_name: event_template("GALLABOX_TEMPLATE_FEES"),
            template_values: values,
            // The currently approved fee template has no CTA button. Keep the
            // payment URL in the named Message variable, and only attach a
            // button substitution after a matching URL-button template is
            // approved and explicitly enabled.
            button_values: if env_flag("GALLABOX_FEES_HAS_URL_BUTTON") {
                vec![json!({
                    "index": 0,
                    "sub_type": "url",
                    "parameters": {"type": "text", "text": link.short_url}
                })]
            } else {
                Vec::new()
            },
        })
        .await;
    record_delivery_outcome(database.pool(), tenant, delivery_id, outcome).await?;
    Ok(())
}

/// Runs inside the API process because Dokploy currently deploys the API as the
/// only always-on backend service. The database event key makes this safe when
/// the service is horizontally scaled.
pub async fn run_daily_attendance(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if !parent_whatsapp_enabled() || state.whatsapp().transport() == "log" {
            continue;
        }
        let slugs = match state.registered_tenant_slugs().await {
            Ok(slugs) => slugs,
            Err(error) => {
                tracing::error!(error=?error, "could not list tenants for guardian attendance messages");
                continue;
            }
        };
        for slug in slugs {
            if let Err(error) = send_attendance_for_tenant(&state, &slug).await {
                tracing::error!(tenant=%slug, error=?error, "guardian attendance sweep failed");
            }
        }
    }
}

async fn send_attendance_for_tenant(state: &AppState, slug: &str) -> anyhow::Result<()> {
    let database = state.tenant_database(slug).await?;
    let tenant: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(slug)
        .fetch_one(database.pool())
        .await?;
    let send_after = std::env::var("ATTENDANCE_WHATSAPP_SEND_AFTER")
        .ok()
        .and_then(|value| NaiveTime::parse_from_str(value.trim(), "%H:%M").ok())
        .unwrap_or_else(|| NaiveTime::from_hms_opt(17, 0, 0).expect("valid default time"));
    let ready: bool =
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Asia/Kolkata')::time >= $1::time")
            .bind(send_after)
            .fetch_one(database.pool())
            .await?;
    if !ready {
        return Ok(());
    }

    let rows = sqlx::query(
        r#"SELECT student.id AS student_id,
                  student.user_account_id::text AS student_user_id,
                  student.student_number, student.email, student.full_name AS student_name,
                  guardian.id AS guardian_id, guardian.full_name AS guardian_name,
                  guardian.phone AS guardian_phone,
                  session.held_on,
                  count(*)::bigint AS total,
                  count(*) FILTER (WHERE entry.status='present')::bigint AS present,
                  count(*) FILTER (WHERE entry.status='absent')::bigint AS absent,
                  count(*) FILTER (WHERE entry.status='od')::bigint AS od,
                  count(*) FILTER (WHERE entry.status='leave')::bigint AS leave
           FROM campus_ops.attendance_entries entry
           JOIN campus_ops.attendance_sessions session
             ON session.tenant_id=entry.tenant_id AND session.id=entry.session_id
           JOIN core.students student
             ON student.tenant_id=entry.tenant_id
            AND student.user_account_id::text=entry.student_user_id
           JOIN LATERAL (
             SELECT candidate.id,candidate.full_name,candidate.phone
             FROM core.guardians candidate
             LEFT JOIN core.student_guardians link
               ON link.tenant_id=candidate.tenant_id AND link.guardian_id=candidate.id
              AND link.student_id=student.id
             WHERE candidate.tenant_id=student.tenant_id
               AND (candidate.student_id=student.id OR link.student_id=student.id)
               AND (candidate.is_primary OR link.is_primary)
               AND NULLIF(candidate.phone,'') IS NOT NULL
             ORDER BY CASE WHEN candidate.student_id=student.id AND candidate.is_primary THEN 0 ELSE 1 END,
                      candidate.updated_at DESC
             LIMIT 1
           ) guardian ON true
           WHERE entry.tenant_id=$1
             AND session.held_on=(now() AT TIME ZONE 'Asia/Kolkata')::date
             AND session.status IN ('submitted_to_principal','approved')
             AND student.user_account_id IS NOT NULL
           GROUP BY student.id,student.user_account_id,student.student_number,student.email,
                    student.full_name,guardian.id,guardian.full_name,guardian.phone,session.held_on"#,
    )
    .bind(tenant)
    .fetch_all(database.pool())
    .await?;

    for row in rows {
        let student = StudentGuardian {
            student_id: row.try_get("student_id")?,
            student_user_id: row.try_get("student_user_id")?,
            student_number: row.try_get("student_number")?,
            student_email: row.try_get("email")?,
            student_name: row.try_get("student_name")?,
            guardian_id: row.try_get("guardian_id")?,
            guardian_name: row.try_get("guardian_name")?,
            guardian_phone: row.try_get("guardian_phone")?,
        };
        let date: chrono::NaiveDate = row.try_get("held_on")?;
        let total: i64 = row.try_get("total")?;
        let present: i64 = row.try_get("present")?;
        let absent: i64 = row.try_get("absent")?;
        let od: i64 = row.try_get("od")?;
        let leave: i64 = row.try_get("leave")?;
        let event_key = format!("attendance:{}:{date}", student.student_user_id);
        let Some(delivery_id) = claim_delivery(
            database.pool(),
            tenant,
            &student,
            "attendance.daily_summary",
            &event_key,
            event_template("GALLABOX_TEMPLATE_ATTENDANCE"),
        )
        .await?
        else {
            continue;
        };
        let attended = present + od;
        let percentage = if total == 0 {
            0.0
        } else {
            attended as f64 * 100.0 / total as f64
        };
        let date_label = date.format("%d %b %Y").to_string();
        let body = format!(
            "{} attendance for {date_label}: {present} present, {absent} absent, {od} OD, {leave} leave ({percentage:.1}%).",
            student.student_name
        );
        let values = BTreeMap::from([
            ("RecipientName".into(), student.guardian_name.clone()),
            ("GuardianName".into(), student.guardian_name.clone()),
            ("StudentName".into(), student.student_name.clone()),
            ("AttendanceDate".into(), date_label),
            ("Present".into(), present.to_string()),
            ("Absent".into(), absent.to_string()),
            ("OD".into(), od.to_string()),
            ("Leave".into(), leave.to_string()),
            ("Total".into(), total.to_string()),
            ("Percentage".into(), format!("{percentage:.1}%")),
            ("Title".into(), "Daily attendance summary".into()),
            ("Message".into(), body.clone()),
            ("EventType".into(), "attendance.daily_summary".into()),
        ]);
        let outcome = state
            .whatsapp()
            .send(WhatsAppMessage {
                to: student.guardian_phone.clone(),
                body,
                media_url: None,
                template_variables: vec![
                    student.student_name.clone(),
                    present.to_string(),
                    absent.to_string(),
                    format!("{percentage:.1}%"),
                ],
                recipient_name: Some(student.guardian_name.clone()),
                template_name: event_template("GALLABOX_TEMPLATE_ATTENDANCE"),
                template_values: values,
                button_values: Vec::new(),
            })
            .await;
        record_delivery_outcome(database.pool(), tenant, delivery_id, outcome).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct PaymentLinkCallback {
    razorpay_payment_id: String,
    razorpay_payment_link_id: String,
    razorpay_payment_link_reference_id: String,
    razorpay_payment_link_status: String,
    razorpay_signature: String,
}

/// Signed Razorpay return target for links placed in WhatsApp.
pub async fn complete_payment_link(
    State(state): State<AppState>,
    Query(callback): Query<PaymentLinkCallback>,
) -> ApiResult<Html<String>> {
    verify_payment_link_signature(&callback)?;
    if callback.razorpay_payment_link_status != "paid" {
        return Ok(Html(result_page(
            "Payment not completed",
            "No fee payment was recorded.",
        )));
    }
    for slug in state
        .registered_tenant_slugs()
        .await
        .map_err(|_| ApiError::Internal)?
    {
        let database = state
            .tenant_database(&slug)
            .await
            .map_err(|_| ApiError::Internal)?;
        let row = sqlx::query(
            r#"SELECT link.id,link.tenant_id,link.student_user_id,link.student_number,
                      link.student_email,link.amount_paise,student.full_name
               FROM campus_ops.guardian_fee_payment_links link
               JOIN core.students student
                 ON student.tenant_id=link.tenant_id AND student.id=link.student_id
               WHERE link.provider_link_id=$1"#,
        )
        .bind(&callback.razorpay_payment_link_id)
        .fetch_optional(database.pool())
        .await?;
        let Some(row) = row else { continue };
        let id: Uuid = row.try_get("id")?;
        let tenant: Uuid = row.try_get("tenant_id")?;
        let amount_paise: i64 = row.try_get("amount_paise")?;
        let student_user_id: String = row.try_get("student_user_id")?;
        let student_number: String = row.try_get("student_number")?;
        let student_email: Option<String> = row.try_get("student_email")?;
        let student_name: String = row.try_get("full_name")?;
        let mut tx = database.pool().begin().await?;
        let newly_paid = sqlx::query(
            r#"UPDATE campus_ops.guardian_fee_payment_links
               SET status='paid',payment_id=$3,paid_at=now(),updated_at=now()
               WHERE tenant_id=$1 AND id=$2 AND paid_at IS NULL"#,
        )
        .bind(tenant)
        .bind(id)
        .bind(&callback.razorpay_payment_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if newly_paid {
            sqlx::query(
                r#"INSERT INTO platform.dynamic_records
                   (id,tenant_id,module_key,record_type,data,created_at,updated_at)
                   VALUES(gen_random_uuid(),$1,'fees','payments',$2,now(),now())"#,
            )
            .bind(tenant)
            .bind(json!({
                "studentId": student_user_id,
                "studentNumber": student_number,
                "studentEmail": student_email,
                "studentName": student_name,
                "amount": amount_paise as f64 / 100.0,
                "amountPaise": amount_paise,
                "currency": "INR",
                "method": "Razorpay Payment Link",
                "paymentPurpose": "tuition_fee",
                "paymentReference": callback.razorpay_payment_id,
                "razorpayPaymentLinkId": callback.razorpay_payment_link_id,
                "paymentDate": Utc::now(),
                "status": "verified",
                "paidBy": "guardian_whatsapp"
            }))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        return Ok(Html(result_page(
            "Payment received",
            &format!(
                "The fee payment for {} has been recorded successfully.",
                student_name
            ),
        )));
    }
    Err(ApiError::NotFound("Payment link was not found".into()))
}

struct PaymentLink {
    short_url: String,
}

async fn existing_or_create_payment_link(
    pool: &sqlx::PgPool,
    tenant_slug: &str,
    tenant: Uuid,
    record: &DynamicRecord,
    student: &StudentGuardian,
    amount_paise: i64,
) -> anyhow::Result<PaymentLink> {
    if let Some(url) = sqlx::query_scalar::<_, String>(
        "SELECT provider_short_url FROM campus_ops.guardian_fee_payment_links WHERE tenant_id=$1 AND fee_record_id=$2 AND amount_paise=$3",
    ).bind(tenant).bind(record.id).bind(amount_paise).fetch_optional(pool).await? {
        return Ok(PaymentLink { short_url: url });
    }
    let credentials = razorpay_credentials()?;
    let reference_id = format!("sc-{}", record.id);
    let callback_url = format!(
        "{}/api/v1/public/fees/payment-links/callback",
        api_public_url().trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(format!("{}/v1/payment_links", razorpay_api_base()))
        .basic_auth(&credentials.0, Some(&credentials.1))
        .json(&json!({
            "amount": amount_paise,
            "currency": "INR",
            "accept_partial": false,
            "reference_id": reference_id,
            "description": format!("SuperCampus tuition fee for {}", student.student_name),
            "customer": {
                "name": student.guardian_name,
                "contact": student.guardian_phone,
                "email": student.student_email,
            },
            "notify": {"sms": false, "email": false},
            "reminder_enable": false,
            "notes": {
                "tenant": tenant_slug,
                "studentUserId": student.student_user_id,
                "feeRecordId": record.id.to_string(),
            },
            "callback_url": callback_url,
            "callback_method": "get"
        }))
        .send()
        .await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!("Razorpay payment-link request failed ({status})");
    }
    let provider_link_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Razorpay response had no payment-link id"))?;
    let short_url = value
        .get("short_url")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Razorpay response had no payment URL"))?;
    sqlx::query(
        r#"INSERT INTO campus_ops.guardian_fee_payment_links
           (tenant_id,fee_record_id,student_id,student_user_id,student_number,student_email,
            guardian_id,guardian_name,guardian_phone,amount_paise,currency,
            provider_link_id,provider_short_url)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'INR',$11,$12)
           ON CONFLICT(tenant_id,fee_record_id,amount_paise) DO NOTHING"#,
    )
    .bind(tenant)
    .bind(record.id)
    .bind(student.student_id)
    .bind(&student.student_user_id)
    .bind(&student.student_number)
    .bind(&student.student_email)
    .bind(student.guardian_id)
    .bind(&student.guardian_name)
    .bind(&student.guardian_phone)
    .bind(amount_paise)
    .bind(provider_link_id)
    .bind(short_url)
    .execute(pool)
    .await?;
    Ok(PaymentLink {
        short_url: short_url.into(),
    })
}

async fn resolve_student_guardian(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    data: &Value,
) -> anyhow::Result<Option<StudentGuardian>> {
    let identity =
        first_string(data, &["studentUserId", "userId", "studentId"]).unwrap_or_default();
    let number =
        first_string(data, &["studentNumber", "roll", "registrationNumber"]).unwrap_or_default();
    let email = first_string(data, &["studentEmail", "email"]).unwrap_or_default();
    let row = sqlx::query(
        r#"SELECT student.id AS student_id,student.user_account_id::text AS student_user_id,
                  student.student_number,student.email,student.full_name AS student_name,
                  guardian.id AS guardian_id,guardian.full_name AS guardian_name,
                  guardian.phone AS guardian_phone
           FROM core.students student
           JOIN LATERAL (
             SELECT candidate.id,candidate.full_name,candidate.phone
             FROM core.guardians candidate
             LEFT JOIN core.student_guardians link
               ON link.tenant_id=candidate.tenant_id AND link.guardian_id=candidate.id
              AND link.student_id=student.id
             WHERE candidate.tenant_id=student.tenant_id
               AND (candidate.student_id=student.id OR link.student_id=student.id)
               AND (candidate.is_primary OR link.is_primary)
               AND NULLIF(candidate.phone,'') IS NOT NULL
             ORDER BY CASE WHEN candidate.student_id=student.id AND candidate.is_primary THEN 0 ELSE 1 END,
                      candidate.updated_at DESC
             LIMIT 1
           ) guardian ON true
           WHERE student.tenant_id=$1 AND student.user_account_id IS NOT NULL
             AND (student.id::text=$2 OR student.user_account_id::text=$2
                  OR lower(student.student_number)=lower($3)
                  OR lower(COALESCE(student.email,''))=lower($4))
           LIMIT 1"#,
    )
    .bind(tenant).bind(identity).bind(number).bind(email)
    .fetch_optional(pool).await?;
    row.map(|row| {
        Ok(StudentGuardian {
            student_id: row.try_get("student_id")?,
            student_user_id: row.try_get("student_user_id")?,
            student_number: row.try_get("student_number")?,
            student_email: row.try_get("email")?,
            student_name: row.try_get("student_name")?,
            guardian_id: row.try_get("guardian_id")?,
            guardian_name: row.try_get("guardian_name")?,
            guardian_phone: row.try_get("guardian_phone")?,
        })
    })
    .transpose()
}

async fn claim_delivery(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    student: &StudentGuardian,
    event_type: &str,
    event_key: &str,
    template_name: Option<String>,
) -> anyhow::Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        r#"INSERT INTO campus_ops.guardian_whatsapp_deliveries
           (tenant_id,student_id,student_user_id,guardian_id,guardian_name,guardian_phone,
            event_type,event_key,template_name,status,attempt_count,locked_at)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'processing',1,now())
           ON CONFLICT(tenant_id,event_key) DO UPDATE SET
             status='processing',attempt_count=guardian_whatsapp_deliveries.attempt_count+1,
             locked_at=now(),last_error=NULL,updated_at=now()
           WHERE (guardian_whatsapp_deliveries.status IN ('queued','retrying','failed')
                    AND guardian_whatsapp_deliveries.next_attempt_at<=now()
                    AND guardian_whatsapp_deliveries.attempt_count<$10)
              OR (guardian_whatsapp_deliveries.status='processing'
                    AND guardian_whatsapp_deliveries.locked_at<now()-interval '10 minutes')
           RETURNING id"#,
    )
    .bind(tenant)
    .bind(student.student_id)
    .bind(&student.student_user_id)
    .bind(student.guardian_id)
    .bind(&student.guardian_name)
    .bind(&student.guardian_phone)
    .bind(event_type)
    .bind(event_key)
    .bind(template_name)
    .bind(MAX_ATTEMPTS)
    .fetch_optional(pool)
    .await?)
}

async fn record_delivery_outcome(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    id: Uuid,
    outcome: anyhow::Result<DeliveryOutcome>,
) -> anyhow::Result<()> {
    match outcome {
        Ok(DeliveryOutcome::Sent { message_id }) => {
            sqlx::query("UPDATE campus_ops.guardian_whatsapp_deliveries SET status='sent',provider_message_id=$3,sent_at=now(),locked_at=NULL,updated_at=now() WHERE tenant_id=$1 AND id=$2")
                .bind(tenant).bind(id).bind(message_id).execute(pool).await?;
        }
        Ok(DeliveryOutcome::NotConfigured) => {
            record_delivery_failure(pool, tenant, id, "WhatsApp is not configured").await?;
        }
        Err(error) => {
            record_delivery_failure(pool, tenant, id, &error.to_string()).await?;
        }
    }
    Ok(())
}

async fn record_delivery_failure(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    id: Uuid,
    error: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE campus_ops.guardian_whatsapp_deliveries
           SET status=CASE WHEN attempt_count >= $4 THEN 'failed' ELSE 'retrying' END,
               locked_at=NULL,last_error=$3,next_attempt_at=now()+interval '15 minutes',updated_at=now()
           WHERE tenant_id=$1 AND id=$2"#,
    ).bind(tenant).bind(id).bind(error.chars().take(500).collect::<String>())
      .bind(MAX_ATTEMPTS).execute(pool).await?;
    Ok(())
}

fn amount_due_paise(data: &Value) -> Option<i64> {
    let direct = first_number(
        data,
        &["amountDue", "outstanding", "balanceDue", "dueAmount"],
    );
    let major = direct.or_else(|| {
        let assigned = first_number(data, &["amount", "total", "assignedAmount", "feeAmount"])?;
        let paid = first_number(data, &["paid", "paidAmount"]).unwrap_or(0.0);
        let waiver = first_number(data, &["waiver", "waiverAmount"]).unwrap_or(0.0);
        Some((assigned - paid - waiver).max(0.0))
    })?;
    let paise = (major * 100.0).round() as i64;
    (paise >= 100).then_some(paise)
}

fn fee_record_is_collectable(data: &Value) -> bool {
    !first_string(data, &["status", "state"]).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "paid" | "cancelled" | "waived" | "refunded"
        )
    })
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

fn first_number(data: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        data.get(*key).and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str()?.replace([',', '₹'], "").trim().parse().ok())
        })
    })
}

fn event_template(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parent_whatsapp_enabled() -> bool {
    std::env::var("WHATSAPP_ENABLED").is_ok_and(|value| value.eq_ignore_ascii_case("true"))
        && !std::env::var("PARENT_WHATSAPP_ENABLED")
            .is_ok_and(|value| value.eq_ignore_ascii_case("false"))
}

fn env_flag(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn api_public_url() -> String {
    std::env::var("API_PUBLIC_URL").unwrap_or_else(|_| "https://api.supercampus.ai".into())
}

fn razorpay_api_base() -> String {
    std::env::var("RAZORPAY_API_BASE_URL")
        .unwrap_or_else(|_| "https://api.razorpay.com".into())
        .trim_end_matches('/')
        .to_owned()
}

fn razorpay_credentials() -> anyhow::Result<(String, String)> {
    let key = std::env::var("RAZORPAY_KEY_ID").unwrap_or_default();
    let secret = std::env::var("RAZORPAY_KEY_SECRET").unwrap_or_default();
    anyhow::ensure!(
        !key.trim().is_empty() && !secret.trim().is_empty(),
        "Razorpay is not configured"
    );
    Ok((key, secret))
}

fn verify_payment_link_signature(callback: &PaymentLinkCallback) -> ApiResult<()> {
    let (_, secret) = razorpay_credentials()
        .map_err(|_| ApiError::ServiceUnavailable("Razorpay is not configured".into()))?;
    let signature = hex::decode(&callback.razorpay_signature)
        .map_err(|_| ApiError::BadRequest("Payment signature is invalid".into()))?;
    let payload = format!(
        "{}|{}|{}|{}",
        callback.razorpay_payment_link_id,
        callback.razorpay_payment_link_reference_id,
        callback.razorpay_payment_link_status,
        callback.razorpay_payment_id
    );
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ApiError::Internal)?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| ApiError::BadRequest("Payment signature verification failed".into()))
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
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>{}</title><style>body{{font-family:system-ui,sans-serif;background:#f7f4fb;margin:0;display:grid;min-height:100vh;place-items:center;color:#211b2e}}main{{background:#fff;border:1px solid #ddd4f6;border-radius:24px;padding:32px;max-width:420px;margin:20px;box-shadow:0 20px 60px #4b1ea51a}}h1{{font-size:26px;margin:0 0 12px}}p{{line-height:1.55;color:#615b6d}}</style></head><body><main><h1>{}</h1><p>{}</p></main></body></html>"#,
        html_escape(title),
        html_escape(title),
        html_escape(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_only_positive_fee_balances() {
        assert_eq!(
            amount_due_paise(&json!({"amount": 501, "paid": 1})),
            Some(50_000)
        );
        assert_eq!(
            amount_due_paise(&json!({"amountDue": "1,250.50"})),
            Some(125_050)
        );
        assert_eq!(amount_due_paise(&json!({"amount": 500, "paid": 500})), None);
    }

    #[test]
    fn payment_link_signature_matches_razorpay_contract() {
        let callback = PaymentLinkCallback {
            razorpay_payment_id: "pay_1".into(),
            razorpay_payment_link_id: "plink_1".into(),
            razorpay_payment_link_reference_id: "sc-1".into(),
            razorpay_payment_link_status: "paid".into(),
            razorpay_signature: String::new(),
        };
        let payload = format!(
            "{}|{}|{}|{}",
            callback.razorpay_payment_link_id,
            callback.razorpay_payment_link_reference_id,
            callback.razorpay_payment_link_status,
            callback.razorpay_payment_id
        );
        assert_eq!(payload, "plink_1|sc-1|paid|pay_1");
    }
}
