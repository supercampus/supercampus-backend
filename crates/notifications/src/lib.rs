#![forbid(unsafe_code)]

//! SuperCampus notifications platform capability.
//!
//! Delivery sits behind the [`Mailer`] trait so callers never depend on a concrete
//! provider. Two implementations ship today:
//!
//! - [`SmtpMailer`] sends real mail over SMTP and is selected when `SMTP_HOST` is set.
//! - [`LogMailer`] writes the message to the tracing log and is the development default.
//! - [`DisabledMailer`] discards messages when email is explicitly disabled.

pub mod push;
pub mod sms;
pub mod whatsapp;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use serde::Serialize;

pub const CRATE_NAME: &str = "supercampus-notifications";

/// A message addressed to a single recipient.
#[derive(Debug, Clone)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub text_body: String,
    pub html_body: Option<String>,
}

/// Delivery transport for outbound email.
#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, message: EmailMessage) -> anyhow::Result<()>;

    /// Human-readable transport name, used in startup logs.
    fn transport(&self) -> &'static str;
}

/// Development transport. Records the message so a developer can copy the link out
/// of the backend log without configuring SMTP.
pub struct LogMailer;

#[async_trait]
impl Mailer for LogMailer {
    async fn send(&self, message: EmailMessage) -> anyhow::Result<()> {
        tracing::info!(
            to = %message.to,
            subject = %message.subject,
            body = %message.text_body,
            "email not sent: SMTP is not configured, logging message instead"
        );
        Ok(())
    }

    fn transport(&self) -> &'static str {
        "log"
    }
}

/// Explicitly disabled transport. This is safe for production environments that
/// have not provisioned SMTP yet because message contents are never logged.
pub struct DisabledMailer;

#[async_trait]
impl Mailer for DisabledMailer {
    async fn send(&self, _message: EmailMessage) -> anyhow::Result<()> {
        tracing::warn!("email delivery is disabled; message discarded");
        Ok(())
    }

    fn transport(&self) -> &'static str {
        "disabled"
    }
}

/// SMTP configuration resolved from the environment.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    /// Use implicit TLS on connect. When false the client upgrades with STARTTLS.
    pub implicit_tls: bool,
}

/// Production transport backed by `lettre`.
pub struct SmtpMailer {
    transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
}

/// Brevo's transactional-email HTTP transport.
///
/// Brevo API keys (`xkeysib-...`) are not SMTP passwords. Supporting the API
/// directly lets operators use the credential Brevo issues on its API Keys page
/// without misconfiguring the SMTP relay username/password pair.
pub struct BrevoMailer {
    client: reqwest::Client,
    api_key: String,
    sender: BrevoSender,
}

#[derive(Debug, Clone, Serialize)]
struct BrevoSender {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct BrevoRecipient<'a> {
    email: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrevoEmailRequest<'a> {
    sender: &'a BrevoSender,
    to: [BrevoRecipient<'a>; 1],
    subject: &'a str,
    text_content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    html_content: Option<&'a str>,
}

impl BrevoMailer {
    pub fn new(api_key: String, from: String) -> anyhow::Result<Self> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            bail!("BREVO_API_KEY cannot be empty");
        }

        let from = from
            .parse::<lettre::message::Mailbox>()
            .with_context(|| format!("MAIL_FROM is not a valid mailbox: {from}"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to build the Brevo email client")?;

        Ok(Self {
            client,
            api_key,
            sender: BrevoSender {
                email: from.email.to_string(),
                name: from.name,
            },
        })
    }
}

#[async_trait]
impl Mailer for BrevoMailer {
    async fn send(&self, message: EmailMessage) -> anyhow::Result<()> {
        let request = BrevoEmailRequest {
            sender: &self.sender,
            to: [BrevoRecipient { email: &message.to }],
            subject: &message.subject,
            text_content: &message.text_body,
            html_content: message.html_body.as_deref(),
        };

        let response = self
            .client
            .post("https://api.brevo.com/v3/smtp/email")
            .header("api-key", &self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&request)
            .send()
            .await
            .context("Brevo email request failed")?;

        let status = response.status();
        if !status.is_success() {
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read Brevo response".to_owned());
            bail!(
                "Brevo rejected the email request ({status}): {}",
                brevo_error_detail(&response_body)
            );
        }

        Ok(())
    }

    fn transport(&self) -> &'static str {
        "brevo"
    }
}

fn brevo_error_detail(response_body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response_body)
        && let Some(message) = value.get("message").and_then(serde_json::Value::as_str)
    {
        return message.chars().take(512).collect();
    }

    let detail: String = response_body.chars().take(512).collect();
    if detail.trim().is_empty() {
        "Brevo returned an empty error response".to_owned()
    } else {
        detail
    }
}

impl SmtpMailer {
    pub fn new(config: SmtpConfig) -> anyhow::Result<Self> {
        use lettre::{
            AsyncSmtpTransport, Tokio1Executor, transport::smtp::authentication::Credentials,
        };

        let from = config
            .from
            .parse::<lettre::message::Mailbox>()
            .with_context(|| format!("MAIL_FROM is not a valid mailbox: {}", config.from))?;

        let builder = if config.implicit_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .context("failed to build an implicit-TLS SMTP relay")?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .context("failed to build a STARTTLS SMTP relay")?
        };

        let builder = builder.port(config.port);
        let builder = match (config.username, config.password) {
            (Some(username), Some(password)) => {
                builder.credentials(Credentials::new(username, password))
            }
            (None, None) => builder,
            _ => bail!("SMTP_USER and SMTP_PASSWORD must be set together"),
        };

        Ok(Self {
            transport: builder.build(),
            from,
        })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, message: EmailMessage) -> anyhow::Result<()> {
        use lettre::{AsyncTransport, Message, message::MultiPart, message::header};

        let to = message
            .to
            .parse::<lettre::message::Mailbox>()
            .with_context(|| format!("recipient is not a valid mailbox: {}", message.to))?;

        let builder = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(message.subject);

        let email = match message.html_body {
            Some(html) => {
                builder.multipart(MultiPart::alternative_plain_html(message.text_body, html))
            }
            None => builder
                .header(header::ContentType::TEXT_PLAIN)
                .body(message.text_body),
        }
        .context("failed to assemble the email message")?;

        self.transport
            .send(email)
            .await
            .context("SMTP delivery failed")?;
        Ok(())
    }

    fn transport(&self) -> &'static str {
        "smtp"
    }
}

/// Builds the mailer described by the environment.
///
/// `SMTP_HOST` selects the SMTP transport. Without it the log transport is returned so
/// local development works with no mail server. Outside development a misconfigured
/// SMTP block is a hard error rather than a silent downgrade to logging.
pub fn mailer_from_environment() -> anyhow::Result<Arc<dyn Mailer>> {
    if std::env::var("EMAIL_TRANSPORT")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("disabled"))
    {
        return Ok(Arc::new(DisabledMailer));
    }

    if let Some(api_key) = std::env::var("BREVO_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        let from = std::env::var("MAIL_FROM")
            .context("MAIL_FROM is required when BREVO_API_KEY is set")?;
        return Ok(Arc::new(BrevoMailer::new(api_key, from)?));
    }

    let host = std::env::var("SMTP_HOST")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let Some(host) = host else {
        let environment = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
        if matches!(environment.as_str(), "production" | "staging") {
            bail!("SMTP_HOST is required outside development; refusing to log email contents");
        }
        return Ok(Arc::new(LogMailer));
    };

    let port = match std::env::var("SMTP_PORT") {
        Ok(value) => value
            .trim()
            .parse::<u16>()
            .context("SMTP_PORT must be a port number")?,
        Err(_) => 587,
    };
    // 465 is implicit TLS by convention; 587 and 25 upgrade via STARTTLS.
    let implicit_tls = match std::env::var("SMTP_IMPLICIT_TLS") {
        Ok(value) => matches!(value.trim(), "1" | "true" | "TRUE"),
        Err(_) => port == 465,
    };
    let from = std::env::var("MAIL_FROM").context("MAIL_FROM is required when SMTP_HOST is set")?;

    let config = SmtpConfig {
        host,
        port,
        username: std::env::var("SMTP_USER").ok().filter(|v| !v.is_empty()),
        password: std::env::var("SMTP_PASSWORD")
            .ok()
            .filter(|v| !v.is_empty()),
        from,
        implicit_tls,
    };
    Ok(Arc::new(SmtpMailer::new(config)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_requires_username_and_password_together() {
        let config = SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            username: Some("user".into()),
            password: None,
            from: "SuperCampus <no-reply@example.com>".into(),
            implicit_tls: false,
        };
        assert!(SmtpMailer::new(config).is_err());
    }

    #[test]
    fn smtp_rejects_an_invalid_from_mailbox() {
        let config = SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            username: None,
            password: None,
            from: "not a mailbox".into(),
            implicit_tls: false,
        };
        assert!(SmtpMailer::new(config).is_err());
    }

    #[test]
    fn brevo_parses_named_sender_mailbox() {
        let mailer = BrevoMailer::new(
            "test-api-key".into(),
            "SuperCampus <no-reply@example.com>".into(),
        )
        .expect("valid Brevo config");

        assert_eq!(mailer.sender.name.as_deref(), Some("SuperCampus"));
        assert_eq!(mailer.sender.email, "no-reply@example.com");
        assert_eq!(mailer.transport(), "brevo");
    }

    #[test]
    fn brevo_rejects_an_invalid_sender_mailbox() {
        assert!(BrevoMailer::new("test-api-key".into(), "not a mailbox".into()).is_err());
    }

    #[test]
    fn brevo_error_detail_extracts_the_provider_message() {
        let detail = brevo_error_detail(
            r#"{"code":"unauthorized","message":"Key not found","ignored":"value"}"#,
        );

        assert_eq!(detail, "Key not found");
    }

    #[test]
    fn brevo_error_detail_limits_non_json_responses() {
        let detail = brevo_error_detail(&"x".repeat(700));

        assert_eq!(detail.len(), 512);
    }

    #[tokio::test]
    async fn log_mailer_accepts_a_message() {
        let mailer = LogMailer;
        let result = mailer
            .send(EmailMessage {
                to: "student@example.com".into(),
                subject: "Reset".into(),
                text_body: "link".into(),
                html_body: None,
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(mailer.transport(), "log");
    }

    #[tokio::test]
    async fn disabled_mailer_discards_a_message() {
        let mailer = DisabledMailer;
        let result = mailer
            .send(EmailMessage {
                to: "student@example.com".into(),
                subject: "Reset".into(),
                text_body: "secret reset link".into(),
                html_body: None,
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(mailer.transport(), "disabled");
    }
}
