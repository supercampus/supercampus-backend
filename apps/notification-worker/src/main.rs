#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use anyhow::{Context, bail};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use supercampus_database::{Database, TenantDatabaseManager};
use supercampus_notifications::{
    EmailMessage, Mailer,
    push::{DeliveryOutcome as PushOutcome, PushMessage, PushSender},
    sms::{DeliveryOutcome as SmsOutcome, SmsMessage, SmsSender},
    whatsapp::{DeliveryOutcome as WhatsAppOutcome, WhatsAppMessage, WhatsAppSender},
};
use uuid::Uuid;

const BATCH_SIZE: i64 = 20;
const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug)]
struct DeliveryJob {
    id: Uuid,
    tenant_id: Uuid,
    lead_id: Uuid,
    channel: String,
    subject: Option<String>,
    content: Value,
    attempt_count: i32,
}

#[derive(Debug)]
struct PushDeliveryJob {
    id: Uuid,
    tenant_id: Uuid,
    notification_id: Uuid,
    device_id: Uuid,
    token: String,
    title: String,
    body: String,
    category: String,
    event_type: String,
    priority: String,
    deep_link: Option<String>,
    data: Value,
    attempt_count: i32,
}

struct Transports {
    mailer: Arc<dyn Mailer>,
    sms: Arc<dyn SmsSender>,
    whatsapp: Arc<dyn WhatsAppSender>,
    push: Option<Arc<dyn PushSender>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("notification-worker");
    let control_url = std::env::var("CONTROL_DATABASE_URL")
        .context("CONTROL_DATABASE_URL is required by the notification worker")?;
    let control = Database::connect(&control_url).await?;
    if std::env::var("SKIP_STARTUP_MIGRATIONS")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"))
    {
        tracing::warn!(
            "notification worker migration check skipped; migrations must be managed by the release job"
        );
    } else {
        control.migrate().await?;
    }
    let tenants =
        TenantDatabaseManager::clustered_with_max_connections(control.clone(), &control_url, 2)?;
    let transports = Transports {
        mailer: supercampus_notifications::mailer_from_environment()?,
        sms: supercampus_notifications::sms::sms_from_environment()?,
        whatsapp: supercampus_notifications::whatsapp::whatsapp_from_environment()?,
        push: supercampus_notifications::push::push_from_environment()?,
    };
    tracing::info!(
        email = transports.mailer.transport(),
        sms = transports.sms.transport(),
        whatsapp = transports.whatsapp.transport(),
        push = transports
            .push
            .as_ref()
            .map_or("disabled", |sender| sender.transport()),
        "notification delivery worker started"
    );

    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("notification delivery worker stopping");
                return Ok(());
            }
            _ = interval.tick() => {
                if let Err(error) = process_all_tenants(&control, &tenants, &transports).await {
                    tracing::error!(error = ?error, "notification delivery sweep failed");
                }
            }
        }
    }
}

async fn process_all_tenants(
    control: &Database,
    tenants: &TenantDatabaseManager,
    transports: &Transports,
) -> anyhow::Result<()> {
    let slugs: Vec<String> = sqlx::query_scalar(
        r#"SELECT tenant.slug
           FROM platform.tenant_databases registry
           JOIN platform.tenants tenant ON tenant.id = registry.tenant_id
           WHERE tenant.status = 'active' AND registry.status = 'active'
           ORDER BY tenant.slug"#,
    )
    .fetch_all(control.pool())
    .await
    .context("failed to list notification tenant databases")?;

    for slug in slugs {
        match tenants.tenant(&slug).await {
            Ok(database) => {
                if let Err(error) = process_tenant(&database, &slug, transports).await {
                    tracing::error!(tenant = %slug, error = ?error, "tenant notification batch failed");
                }
            }
            Err(error) => {
                tracing::error!(tenant = %slug, error = ?error, "tenant notification database unavailable");
            }
        }
    }
    Ok(())
}

async fn process_tenant(
    database: &Database,
    tenant_slug: &str,
    transports: &Transports,
) -> anyhow::Result<()> {
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_one(database.pool())
        .await
        .context("tenant identity is missing from its database")?;
    let jobs = claim_batch(database, tenant_id).await?;
    for job in jobs {
        let result = deliver(database, &job, transports).await;
        record_result(database, &job, result).await?;
    }
    if let Some(sender) = transports.push.as_ref() {
        enqueue_push_deliveries(database, tenant_id).await?;
        let jobs = claim_push_batch(database, tenant_id).await?;
        for job in jobs {
            let result = sender
                .send(PushMessage {
                    token: job.token.clone(),
                    title: job.title.clone(),
                    body: job.body.clone(),
                    deep_link: job.deep_link.clone(),
                    category: job.category.clone(),
                    event_type: job.event_type.clone(),
                    priority: job.priority.clone(),
                    data: job.data.clone(),
                })
                .await;
            record_push_result(database, &job, result).await?;
        }
    }
    Ok(())
}

async fn enqueue_push_deliveries(database: &Database, tenant_id: Uuid) -> anyhow::Result<()> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, tenant_id).await?;
    sqlx::query(
        r#"INSERT INTO campus_ops.notification_push_deliveries
             (tenant_id, notification_id, device_id)
           SELECT notification.tenant_id, notification.id, device.id
           FROM campus_ops.notifications notification
           JOIN campus_ops.push_devices device
             ON device.tenant_id=notification.tenant_id
            AND device.enabled AND device.provider='fcm'
           LEFT JOIN campus_ops.notification_preferences preference
             ON preference.tenant_id=notification.tenant_id
            AND preference.user_id=device.user_id
            AND preference.category=notification.category
           WHERE notification.tenant_id=$1
             AND notification.push_status IN ('queued','retrying')
             AND (notification.expires_at IS NULL OR notification.expires_at > now())
             AND COALESCE(preference.push_enabled, true)
             AND (
               notification.priority='urgent'
               OR preference.quiet_hours_start IS NULL
               OR NOT CASE
                 WHEN preference.quiet_hours_start < preference.quiet_hours_end THEN
                   (now() AT TIME ZONE COALESCE((
                     SELECT configuration.timezone
                     FROM core.timetable_configurations configuration
                     WHERE configuration.tenant_id=notification.tenant_id
                       AND configuration.active
                     ORDER BY configuration.updated_at DESC LIMIT 1
                   ), 'Asia/Kolkata'))::time
                     >= preference.quiet_hours_start
                   AND (now() AT TIME ZONE COALESCE((
                     SELECT configuration.timezone
                     FROM core.timetable_configurations configuration
                     WHERE configuration.tenant_id=notification.tenant_id
                       AND configuration.active
                     ORDER BY configuration.updated_at DESC LIMIT 1
                   ), 'Asia/Kolkata'))::time
                     < preference.quiet_hours_end
                 ELSE
                   (now() AT TIME ZONE COALESCE((
                     SELECT configuration.timezone
                     FROM core.timetable_configurations configuration
                     WHERE configuration.tenant_id=notification.tenant_id
                       AND configuration.active
                     ORDER BY configuration.updated_at DESC LIMIT 1
                   ), 'Asia/Kolkata'))::time
                     >= preference.quiet_hours_start
                   OR (now() AT TIME ZONE COALESCE((
                     SELECT configuration.timezone
                     FROM core.timetable_configurations configuration
                     WHERE configuration.tenant_id=notification.tenant_id
                       AND configuration.active
                     ORDER BY configuration.updated_at DESC LIMIT 1
                   ), 'Asia/Kolkata'))::time
                     < preference.quiet_hours_end
               END
             )
             AND (
               notification.recipient_user_id=device.user_id
               OR (
                 notification.recipient_user_id IS NULL
                 AND notification.recipient_role IS NOT NULL
                 AND EXISTS (
                   SELECT 1 FROM authz.user_roles user_role
                   JOIN authz.roles role
                     ON role.tenant_id=user_role.tenant_id
                    AND role.id=user_role.role_id AND role.active
                   WHERE user_role.tenant_id=notification.tenant_id
                     AND user_role.user_id::text=device.user_id
                     AND role.role_key=notification.recipient_role
                 )
               )
             )
           ON CONFLICT(tenant_id,notification_id,device_id) DO NOTHING"#,
    )
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn claim_push_batch(
    database: &Database,
    tenant_id: Uuid,
) -> anyhow::Result<Vec<PushDeliveryJob>> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, tenant_id).await?;
    sqlx::query(
        r#"UPDATE campus_ops.notification_push_deliveries
           SET status='retrying', locked_at=NULL, next_attempt_at=now(),
               last_error=COALESCE(last_error,'delivery lease expired'), updated_at=now()
           WHERE tenant_id=$1 AND status='processing'
             AND locked_at < now() - interval '10 minutes'"#,
    )
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await?;
    let rows = sqlx::query(
        r#"WITH candidates AS (
             SELECT delivery.id
             FROM campus_ops.notification_push_deliveries delivery
             WHERE delivery.tenant_id=$1
               AND delivery.status IN ('queued','retrying')
               AND delivery.next_attempt_at <= now()
             ORDER BY delivery.next_attempt_at,delivery.created_at
             FOR UPDATE SKIP LOCKED LIMIT $2
           ), claimed AS (
             UPDATE campus_ops.notification_push_deliveries delivery
             SET status='processing',locked_at=now(),
                 attempt_count=delivery.attempt_count+1,updated_at=now()
             FROM candidates WHERE delivery.id=candidates.id
             RETURNING delivery.*
           )
           SELECT claimed.id,claimed.tenant_id,claimed.notification_id,claimed.device_id,
             claimed.attempt_count,device.token,notification.title,notification.body,
             notification.category,notification.event_type,notification.priority,
             notification.deep_link,notification.data
           FROM claimed
           JOIN campus_ops.push_devices device ON device.id=claimed.device_id
           JOIN campus_ops.notifications notification ON notification.id=claimed.notification_id"#,
    )
    .bind(tenant_id)
    .bind(BATCH_SIZE)
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    rows.into_iter()
        .map(|row| {
            Ok(PushDeliveryJob {
                id: row.try_get("id")?,
                tenant_id: row.try_get("tenant_id")?,
                notification_id: row.try_get("notification_id")?,
                device_id: row.try_get("device_id")?,
                token: row.try_get("token")?,
                title: row.try_get("title")?,
                body: row.try_get("body")?,
                category: row.try_get("category")?,
                event_type: row.try_get("event_type")?,
                priority: row.try_get("priority")?,
                deep_link: row.try_get("deep_link")?,
                data: row.try_get("data")?,
                attempt_count: row.try_get("attempt_count")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(Into::into)
}

async fn claim_batch(database: &Database, tenant_id: Uuid) -> anyhow::Result<Vec<DeliveryJob>> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, tenant_id).await?;
    sqlx::query(
        r#"UPDATE crm.communications
           SET status = 'retrying', locked_at = NULL, next_attempt_at = now(),
               last_error = coalesce(last_error, 'delivery lease expired'), updated_at = now()
           WHERE tenant_id = $1 AND status = 'processing'
             AND locked_at < now() - interval '10 minutes'"#,
    )
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await?;
    let rows = sqlx::query(
        r#"WITH candidates AS (
               SELECT id
               FROM crm.communications
               WHERE tenant_id = $1 AND direction = 'outbound'
                 AND channel IN ('email', 'sms', 'whatsapp')
                 AND status IN ('queued', 'retrying')
                 AND next_attempt_at <= now()
               ORDER BY next_attempt_at, created_at
               FOR UPDATE SKIP LOCKED
               LIMIT $2
           )
           UPDATE crm.communications communication
           SET status = 'processing', locked_at = now(),
               attempt_count = communication.attempt_count + 1, updated_at = now()
           FROM candidates
           WHERE communication.id = candidates.id
           RETURNING communication.id, communication.tenant_id, communication.lead_id,
                     communication.channel, communication.subject, communication.content,
                     communication.attempt_count"#,
    )
    .bind(tenant_id)
    .bind(BATCH_SIZE)
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    rows.into_iter()
        .map(|row| {
            Ok(DeliveryJob {
                id: row.try_get("id")?,
                tenant_id: row.try_get("tenant_id")?,
                lead_id: row.try_get("lead_id")?,
                channel: row.try_get("channel")?,
                subject: row.try_get("subject")?,
                content: row.try_get("content")?,
                attempt_count: row.try_get("attempt_count")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(Into::into)
}

async fn deliver(
    database: &Database,
    job: &DeliveryJob,
    transports: &Transports,
) -> anyhow::Result<Option<String>> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, job.tenant_id).await?;
    let contact = sqlx::query(
        "SELECT email, phone, whatsapp FROM crm.leads WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(job.tenant_id)
    .bind(job.lead_id)
    .fetch_optional(&mut *transaction)
    .await?
    .context("communication lead no longer exists")?;
    transaction.commit().await?;
    let body = message_body(&job.content)?;
    match job.channel.as_str() {
        "email" => {
            if transports.mailer.transport() == "log" {
                bail!("SMTP is not configured");
            }
            let to: Option<String> = contact.try_get("email")?;
            transports
                .mailer
                .send(EmailMessage {
                    to: to.context("lead has no email address")?,
                    subject: job
                        .subject
                        .clone()
                        .unwrap_or_else(|| "SuperCampus update".into()),
                    text_body: body,
                    html_body: None,
                })
                .await?;
            Ok(None)
        }
        "sms" => {
            let to: Option<String> = contact.try_get("phone")?;
            match transports
                .sms
                .send(SmsMessage {
                    to: to.context("lead has no phone number")?,
                    body,
                })
                .await?
            {
                SmsOutcome::Sent { message_id } => Ok(Some(message_id)),
                SmsOutcome::NotConfigured => bail!("SMS is not configured"),
            }
        }
        "whatsapp" => {
            let whatsapp: Option<String> = contact.try_get("whatsapp")?;
            let phone: Option<String> = contact.try_get("phone")?;
            let to = whatsapp
                .or(phone)
                .context("lead has no WhatsApp or phone number")?;
            let variables = ["applicationUrl", "otp", "expiresAt"]
                .iter()
                .filter_map(|key| {
                    job.content
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect();
            match transports
                .whatsapp
                .send(WhatsAppMessage {
                    to,
                    body,
                    media_url: None,
                    template_variables: variables,
                })
                .await?
            {
                WhatsAppOutcome::Sent { message_id } => Ok(Some(message_id)),
                WhatsAppOutcome::NotConfigured => bail!("WhatsApp is not configured"),
            }
        }
        channel => bail!("unsupported delivery channel {channel}"),
    }
}

async fn record_result(
    database: &Database,
    job: &DeliveryJob,
    result: anyhow::Result<Option<String>>,
) -> anyhow::Result<()> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, job.tenant_id).await?;
    match result {
        Ok(message_id) => {
            sqlx::query(
                r#"UPDATE crm.communications
                   SET status = 'sent', outcome = 'accepted_by_provider', sent_at = now(),
                       provider_message_id = $3, locked_at = NULL, last_error = NULL, updated_at = now()
                   WHERE tenant_id = $1 AND id = $2 AND status = 'processing'"#,
            )
            .bind(job.tenant_id)
            .bind(job.id)
            .bind(message_id)
            .execute(&mut *transaction)
            .await?;
            tracing::info!(communication_id = %job.id, channel = %job.channel, "communication accepted by provider");
        }
        Err(error) => {
            let terminal = job.attempt_count >= MAX_ATTEMPTS;
            let retry_seconds = retry_delay_seconds(job.attempt_count);
            sqlx::query(
                r#"UPDATE crm.communications
                   SET status = $3, locked_at = NULL, last_error = $4,
                       next_attempt_at = now() + make_interval(secs => $5), updated_at = now()
                   WHERE tenant_id = $1 AND id = $2 AND status = 'processing'"#,
            )
            .bind(job.tenant_id)
            .bind(job.id)
            .bind(if terminal { "failed" } else { "retrying" })
            .bind(safe_error(&error))
            .bind(retry_seconds)
            .execute(&mut *transaction)
            .await?;
            tracing::warn!(communication_id = %job.id, channel = %job.channel, attempt = job.attempt_count, terminal, "communication delivery failed");
        }
    }
    transaction.commit().await?;
    Ok(())
}

async fn record_push_result(
    database: &Database,
    job: &PushDeliveryJob,
    result: anyhow::Result<PushOutcome>,
) -> anyhow::Result<()> {
    let mut transaction = database.pool().begin().await?;
    set_tenant(&mut transaction, job.tenant_id).await?;
    match result {
        Ok(PushOutcome::Sent { message_id }) => {
            sqlx::query(
                r#"UPDATE campus_ops.notification_push_deliveries
                   SET status='sent',provider_message_id=$3,sent_at=now(),
                       locked_at=NULL,last_error=NULL,updated_at=now()
                   WHERE tenant_id=$1 AND id=$2 AND status='processing'"#,
            )
            .bind(job.tenant_id)
            .bind(job.id)
            .bind(message_id)
            .execute(&mut *transaction)
            .await?;
            tracing::info!(notification_id=%job.notification_id, device_id=%job.device_id, "push accepted by FCM");
        }
        Ok(PushOutcome::InvalidToken) => {
            sqlx::query(
                r#"UPDATE campus_ops.notification_push_deliveries
                   SET status='invalid',locked_at=NULL,last_error='FCM token is no longer registered',
                       updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='processing'"#,
            )
            .bind(job.tenant_id)
            .bind(job.id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"UPDATE campus_ops.push_devices
                   SET enabled=false,updated_at=now()
                   WHERE tenant_id=$1 AND id=$2"#,
            )
            .bind(job.tenant_id)
            .bind(job.device_id)
            .execute(&mut *transaction)
            .await?;
            tracing::info!(notification_id=%job.notification_id, device_id=%job.device_id, "disabled an invalid FCM token");
        }
        Err(error) => {
            let terminal = job.attempt_count >= MAX_ATTEMPTS;
            sqlx::query(
                r#"UPDATE campus_ops.notification_push_deliveries
                   SET status=$3,locked_at=NULL,last_error=$4,
                       next_attempt_at=now()+make_interval(secs=>$5),updated_at=now()
                   WHERE tenant_id=$1 AND id=$2 AND status='processing'"#,
            )
            .bind(job.tenant_id)
            .bind(job.id)
            .bind(if terminal { "failed" } else { "retrying" })
            .bind(safe_error(&error))
            .bind(retry_delay_seconds(job.attempt_count))
            .execute(&mut *transaction)
            .await?;
            tracing::warn!(notification_id=%job.notification_id, device_id=%job.device_id, attempt=job.attempt_count, terminal, "push delivery failed");
        }
    }

    sqlx::query(
        r#"UPDATE campus_ops.notifications notification
           SET push_status=CASE
                 WHEN EXISTS (
                   SELECT 1 FROM campus_ops.notification_push_deliveries delivery
                   WHERE delivery.notification_id=notification.id
                     AND delivery.status IN ('queued','processing','retrying')
                 ) THEN 'retrying'
                 WHEN EXISTS (
                   SELECT 1 FROM campus_ops.notification_push_deliveries delivery
                   WHERE delivery.notification_id=notification.id AND delivery.status='sent'
                 ) THEN 'sent'
                 ELSE 'failed'
               END,
               push_sent_at=CASE WHEN EXISTS (
                 SELECT 1 FROM campus_ops.notification_push_deliveries delivery
                 WHERE delivery.notification_id=notification.id AND delivery.status='sent'
               ) THEN COALESCE(notification.push_sent_at,now()) ELSE notification.push_sent_at END,
               push_attempt_count=(
                 SELECT COALESCE(MAX(delivery.attempt_count),0)
                 FROM campus_ops.notification_push_deliveries delivery
                 WHERE delivery.notification_id=notification.id
               ),
               push_last_error=(
                 SELECT delivery.last_error
                 FROM campus_ops.notification_push_deliveries delivery
                 WHERE delivery.notification_id=notification.id AND delivery.last_error IS NOT NULL
                 ORDER BY delivery.updated_at DESC LIMIT 1
               )
           WHERE notification.tenant_id=$1 AND notification.id=$2"#,
    )
    .bind(job.tenant_id)
    .bind(job.notification_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn set_tenant(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

fn message_body(content: &Value) -> anyhow::Result<String> {
    if let Some(value) = content.as_str() {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    for key in ["message", "text", "body", "content"] {
        if let Some(value) = content.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_owned());
            }
        }
    }
    bail!("communication content has no message text")
}

fn retry_delay_seconds(attempt: i32) -> i32 {
    let exponent = u32::try_from((attempt - 1).clamp(0, 6)).unwrap_or_default();
    (30_i32.saturating_mul(2_i32.saturating_pow(exponent))).min(3600)
}

fn safe_error(error: &anyhow::Error) -> String {
    error.to_string().chars().take(1000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_supported_message_shapes() {
        assert_eq!(message_body(&json!({"message": "Hello"})).unwrap(), "Hello");
        assert!(message_body(&json!({"otp": "123456"})).is_err());
    }

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_delay_seconds(1), 30);
        assert_eq!(retry_delay_seconds(5), 480);
        assert_eq!(retry_delay_seconds(99), 1920);
    }
}
