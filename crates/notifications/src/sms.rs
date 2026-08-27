//! Outbound SMS delivery through Twilio Programmable Messaging.

use std::sync::Arc;

use anyhow::{Context, bail};
use async_trait::async_trait;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct SmsMessage {
    pub to: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Sent { message_id: String },
    NotConfigured,
}

#[async_trait]
pub trait SmsSender: Send + Sync {
    async fn send(&self, message: SmsMessage) -> anyhow::Result<DeliveryOutcome>;
    fn transport(&self) -> &'static str;
}

pub struct LogSms;

#[async_trait]
impl SmsSender for LogSms {
    async fn send(&self, message: SmsMessage) -> anyhow::Result<DeliveryOutcome> {
        tracing::info!(to = %message.to, body = %message.body, "SMS not sent: Twilio SMS is not configured");
        Ok(DeliveryOutcome::NotConfigured)
    }

    fn transport(&self) -> &'static str {
        "log"
    }
}

#[derive(Debug, Clone)]
struct TwilioSmsConfig {
    account_sid: String,
    username: String,
    password: String,
    from: Option<String>,
    messaging_service_sid: Option<String>,
}

pub struct TwilioSms {
    config: TwilioSmsConfig,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct TwilioResponse {
    sid: Option<String>,
    message: Option<String>,
    code: Option<i64>,
}

#[async_trait]
impl SmsSender for TwilioSms {
    async fn send(&self, message: SmsMessage) -> anyhow::Result<DeliveryOutcome> {
        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.config.account_sid
        );
        let mut form = vec![("To", normalise(&message.to)), ("Body", message.body)];
        if let Some(service) = self.config.messaging_service_sid.as_ref() {
            form.push(("MessagingServiceSid", service.clone()));
        } else if let Some(from) = self.config.from.as_ref() {
            form.push(("From", normalise(from)));
        }

        let response = self
            .client
            .post(endpoint)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .form(&form)
            .send()
            .await
            .context("Twilio SMS could not be reached")?;
        let status = response.status();
        let payload: TwilioResponse = response.json().await.unwrap_or(TwilioResponse {
            sid: None,
            message: None,
            code: None,
        });
        if !status.is_success() {
            bail!(
                "Twilio rejected the SMS ({}): {}{}",
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

pub fn sms_from_environment() -> anyhow::Result<Arc<dyn SmsSender>> {
    let account_sid = optional("TWILIO_ACCOUNT_SID");
    let auth_token = optional("TWILIO_AUTH_TOKEN");
    let api_key_sid = optional("TWILIO_API_KEY_SID");
    let api_key_secret = optional("TWILIO_API_KEY_SECRET");
    let from = optional("TWILIO_SMS_FROM");
    let messaging_service_sid = optional("TWILIO_MESSAGING_SERVICE_SID");

    let configured = account_sid.is_some()
        || auth_token.is_some()
        || api_key_sid.is_some()
        || api_key_secret.is_some()
        || from.is_some()
        || messaging_service_sid.is_some();
    if !configured {
        let environment = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
        if matches!(environment.as_str(), "production" | "staging") {
            bail!("Twilio SMS configuration is required outside development");
        }
        return Ok(Arc::new(LogSms));
    }

    let account_sid = account_sid.context("TWILIO_ACCOUNT_SID is required for SMS")?;
    if from.is_none() && messaging_service_sid.is_none() {
        bail!("TWILIO_SMS_FROM or TWILIO_MESSAGING_SERVICE_SID is required for SMS");
    }
    let (username, password) = match (api_key_sid, api_key_secret, auth_token) {
        (Some(key), Some(secret), _) => (key, secret),
        (None, None, Some(token)) => (account_sid.clone(), token),
        _ => bail!("configure an API key SID/secret pair or TWILIO_AUTH_TOKEN for SMS"),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("failed to build the Twilio SMS client")?;
    Ok(Arc::new(TwilioSms {
        config: TwilioSmsConfig {
            account_sid,
            username,
            password,
            from,
            messaging_service_sid,
        },
        client,
    }))
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn normalise(number: &str) -> String {
    let digits: String = number
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return String::new();
    }
    format!("+{digits}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_phone_numbers_to_e164_shape() {
        assert_eq!(normalise("+91 98765-43210"), "+919876543210");
    }
}
