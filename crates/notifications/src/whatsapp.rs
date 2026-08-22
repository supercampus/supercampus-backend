//! Outbound WhatsApp, over Twilio's Programmable Messaging API.
//!
//! Visitors have no account and no app. A parent arriving to see their child,
//! or a guest arriving for a meeting, gets their gate pass as an image in a
//! WhatsApp thread and shows it at the gate — so this is the only channel by
//! which those passes reach anyone, not a convenience on top of one.
//!
//! The transport mirrors [`crate::Mailer`]: a real sender when the environment
//! is configured, and one that records what it would have sent when it is not.
//! That keeps the approval flow complete and testable on a machine with no
//! Twilio account, and turns into real delivery the moment credentials land.

use std::sync::Arc;

use anyhow::{Context, bail};
use async_trait::async_trait;
use serde::Deserialize;

/// One outbound WhatsApp message.
#[derive(Debug, Clone)]
pub struct WhatsAppMessage {
    /// E.164, without the `whatsapp:` prefix — the transport adds it.
    pub to: String,
    pub body: String,
    /// A publicly reachable image URL. Twilio fetches it, so it cannot be a
    /// signed URL that expires before the fetch.
    pub media_url: Option<String>,
    /// Ordered substitutions for an approved template, used only when one is
    /// configured. WhatsApp numbers them from 1.
    pub template_variables: Vec<String>,
}

/// The outcome of an attempt, which is not the same as whether it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Accepted by the provider.
    Sent { message_id: String },
    /// No transport is configured, so nothing left the building.
    NotConfigured,
}

#[async_trait]
pub trait WhatsAppSender: Send + Sync {
    async fn send(&self, message: WhatsAppMessage) -> anyhow::Result<DeliveryOutcome>;

    /// Human-readable transport name, used in startup logs.
    fn transport(&self) -> &'static str;
}

/// Development transport. Records the message rather than sending it.
///
/// It returns `NotConfigured` rather than `Sent` on purpose: a pass that never
/// reached a visitor's phone must not be recorded as delivered, or an
/// administrator will stand at a gate wondering why the guest has nothing to
/// show.
pub struct LogWhatsApp;

#[async_trait]
impl WhatsAppSender for LogWhatsApp {
    async fn send(&self, message: WhatsAppMessage) -> anyhow::Result<DeliveryOutcome> {
        tracing::info!(
            to = %message.to,
            media = message.media_url.as_deref().unwrap_or("none"),
            body = %message.body,
            "WhatsApp not sent: Twilio is not configured, logging message instead"
        );
        Ok(DeliveryOutcome::NotConfigured)
    }

    fn transport(&self) -> &'static str {
        "log"
    }
}

/// Twilio credentials.
///
/// Twilio accepts two basic-auth pairs on the Messages endpoint, and this
/// supports both:
///
/// * **API Key SID + secret** — preferred, because a key can be revoked on its
///   own without rotating everything else. A *restricted* key must have
///   Messaging permissions attached or every call fails with 70051.
/// * **Account SID + auth token** — the account's own credentials, which work
///   with no policy setup but carry every privilege the account has.
///
/// The Account SID is required either way: it is the path segment the message
/// is posted to, and it cannot be derived from a key.
#[derive(Debug, Clone)]
pub struct TwilioConfig {
    pub account_sid: String,
    /// Basic-auth username: an API key SID, or the account SID again.
    pub username: String,
    /// Basic-auth password: the API key secret, or the account's auth token.
    pub password: String,
    /// The sender, e.g. `+14155238886`. Stored without the `whatsapp:` prefix.
    pub whatsapp_from: String,
    /// An approved WhatsApp template (`HX…`).
    ///
    /// WhatsApp only allows freeform text inside the 24 hours after the
    /// recipient last wrote to you; outside it, a template is the only thing
    /// that will be delivered. Twilio *trial* accounts go further and refuse
    /// freeform entirely (`ContentSid Required`, 21654), so on a trial this is
    /// not an optimisation — it is the only way a message arrives at all.
    pub content_sid: Option<String>,
}

pub struct TwilioWhatsApp {
    config: TwilioConfig,
    client: reqwest::Client,
}

impl TwilioWhatsApp {
    pub fn new(config: TwilioConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .context("failed to build the Twilio HTTP client")?;
        Ok(Self { config, client })
    }
}

#[derive(Deserialize)]
struct TwilioResponse {
    sid: Option<String>,
    message: Option<String>,
    code: Option<i64>,
}

#[async_trait]
impl WhatsAppSender for TwilioWhatsApp {
    async fn send(&self, message: WhatsAppMessage) -> anyhow::Result<DeliveryOutcome> {
        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.config.account_sid
        );

        let mut form = vec![
            ("To", format!("whatsapp:{}", normalise(&message.to))),
            (
                "From",
                format!("whatsapp:{}", normalise(&self.config.whatsapp_from)),
            ),
        ];

        match self.config.content_sid.as_ref() {
            Some(content_sid) => {
                // A template carries its own wording, so Body must not be sent
                // alongside it — Twilio rejects the pair.
                form.push(("ContentSid", content_sid.clone()));
                if !message.template_variables.is_empty() {
                    let variables: std::collections::BTreeMap<String, String> = message
                        .template_variables
                        .iter()
                        .enumerate()
                        .map(|(index, value)| ((index + 1).to_string(), value.clone()))
                        .collect();
                    if let Ok(encoded) = serde_json::to_string(&variables) {
                        form.push(("ContentVariables", encoded));
                    }
                }
            }
            None => {
                form.push(("Body", message.body.clone()));
                // Trial accounts reject MediaUrl outright, so it only ever goes
                // out on the freeform path where the account is already known to
                // be a paid one.
                if let Some(media) = message.media_url.as_ref() {
                    form.push(("MediaUrl", media.clone()));
                }
            }
        }

        let response = self
            .client
            .post(endpoint)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .form(&form)
            .send()
            .await
            .context("Twilio could not be reached")?;

        let status = response.status();
        let payload: TwilioResponse = response.json().await.unwrap_or(TwilioResponse {
            sid: None,
            message: None,
            code: None,
        });

        if !status.is_success() {
            // Twilio's own error text is far more useful than the status code —
            // it names the unverified number or the unapproved template.
            bail!(
                "Twilio rejected the message ({}): {}{}",
                status.as_u16(),
                payload.message.unwrap_or_else(|| "no detail".into()),
                payload
                    .code
                    .map(|code| format!(" [{code}]"))
                    .unwrap_or_default()
            );
        }

        Ok(DeliveryOutcome::Sent {
            message_id: payload.sid.unwrap_or_default(),
        })
    }

    fn transport(&self) -> &'static str {
        "twilio"
    }
}

/// Twilio wants `+<country><number>` and tolerates nothing else.
fn normalise(number: &str) -> String {
    let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return String::new();
    }
    format!("+{digits}")
}

/// Builds the transport the environment is configured for.
///
/// An account SID, a sender, and one of the two credential pairs are all
/// required. A half-configured Twilio account is the normal state while
/// credentials are still being gathered, and falling back to logging while
/// *looking* configured is worse than naming the missing piece out loud.
pub fn whatsapp_from_environment() -> anyhow::Result<Arc<dyn WhatsAppSender>> {
    fn present(key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    let account_sid = present("TWILIO_ACCOUNT_SID");
    let auth_token = present("TWILIO_AUTH_TOKEN");
    let api_key_sid = present("TWILIO_API_KEY_SID");
    let api_key_secret = present("TWILIO_API_KEY_SECRET");
    let whatsapp_from = present("TWILIO_WHATSAPP_FROM");
    let content_sid = present("TWILIO_WHATSAPP_CONTENT_SID");

    // An auth token wins when both are present: it is the pair an operator
    // reaches for when a restricted key has turned out to have no permissions,
    // and silently preferring the broken key would waste the fix.
    let credentials = match (&auth_token, &api_key_sid, &api_key_secret) {
        (Some(token), _, _) => account_sid
            .as_ref()
            .map(|sid| (sid.clone(), token.clone())),
        (None, Some(sid), Some(secret)) => Some((sid.clone(), secret.clone())),
        _ => None,
    };

    let configured = [
        ("TWILIO_ACCOUNT_SID", &account_sid),
        ("TWILIO_WHATSAPP_FROM", &whatsapp_from),
        ("TWILIO_AUTH_TOKEN or TWILIO_API_KEY_SID+SECRET", &credentials.as_ref().map(|_| String::new())),
    ];
    let missing: Vec<&str> = configured
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| *key)
        .collect();

    if missing.len() == configured.len() {
        return Ok(Arc::new(LogWhatsApp));
    }

    if !missing.is_empty() {
        let environment = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
        if matches!(environment.as_str(), "production" | "staging") {
            bail!(
                "Twilio is partially configured; missing {}",
                missing.join(", ")
            );
        }
        tracing::warn!(
            missing = %missing.join(", "),
            "Twilio is partially configured; visitor passes will be logged instead of sent"
        );
        return Ok(Arc::new(LogWhatsApp));
    }

    let (username, password) = credentials.expect("checked above");
    Ok(Arc::new(TwilioWhatsApp::new(TwilioConfig {
        account_sid: account_sid.expect("checked above"),
        username,
        password,
        whatsapp_from: whatsapp_from.expect("checked above"),
        content_sid,
    })?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_normalised_to_e164() {
        assert_eq!(normalise("+91 98765 43210"), "+919876543210");
        assert_eq!(normalise("919876543210"), "+919876543210");
        assert_eq!(normalise("(91) 98765-43210"), "+919876543210");
        assert_eq!(normalise(""), "");
    }

    #[tokio::test]
    async fn the_log_transport_reports_that_nothing_was_sent() {
        // The distinction that matters: a logged pass is not a delivered pass.
        let outcome = LogWhatsApp
            .send(WhatsAppMessage {
                to: "+919876543210".into(),
                body: "pass".into(),
                media_url: None,
                template_variables: Vec::new(),
            })
            .await
            .expect("log transport never fails");
        assert_eq!(outcome, DeliveryOutcome::NotConfigured);
    }
}
