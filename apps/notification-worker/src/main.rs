#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use anyhow::{Context, bail};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use supercampus_database::{Database, TenantDatabaseManager};
use supercampus_notifications::{
    EmailMessage, Mailer,
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

struct Transports {
    mailer: Arc<dyn Mailer>,
    sms: Arc<dyn SmsSender>,
    whatsapp: Arc<dyn WhatsAppSender>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("notification-worker");
    let control_url = std::env::var("CONTROL_DATABASE_URL")
        .context("CONTROL_DATABASE_URL is required by the notification worker")?;
    let control = Database::connect(&control_url).await?;
    control.migrate().await?;
    let tenants =
        TenantDatabaseManager::clustered_with_max_connections(control.clone(), &control_url, 2)?;
    let transports = Transports {
        mailer: supercampus_notifications::mailer_from_environment()?,
        sms: supercampus_notifications::sms::sms_from_environment()?,
        whatsapp: supercampus_notifications::whatsapp::whatsapp_from_environment()?,
    };
    tracing::info!(
        email = transports.mailer.transport(),
        sms = transports.sms.transport(),
        whatsapp = transports.whatsapp.transport(),
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
    Ok(())
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
