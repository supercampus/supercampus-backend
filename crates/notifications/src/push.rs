//! Firebase Cloud Messaging transport using the HTTP v1 API.
//!
//! Credentials are loaded from `GOOGLE_APPLICATION_CREDENTIALS`; private key
//! material is never logged or embedded in the application image.

use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Context, bail};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const FIREBASE_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

#[derive(Debug, Clone)]
pub struct PushMessage {
    pub token: String,
    pub title: String,
    pub body: String,
    pub deep_link: Option<String>,
    pub category: String,
    pub event_type: String,
    pub priority: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Sent { message_id: String },
    InvalidToken,
}

#[async_trait]
pub trait PushSender: Send + Sync {
    async fn send(&self, message: PushMessage) -> anyhow::Result<DeliveryOutcome>;
    fn transport(&self) -> &'static str;
}

#[derive(Debug, Deserialize)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

#[derive(Debug, Serialize)]
struct TokenClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

#[derive(Debug)]
struct CachedToken {
    value: String,
    expires_at: DateTime<Utc>,
}

pub struct FcmPushSender {
    client: Client,
    project_id: String,
    client_email: String,
    token_uri: String,
    encoding_key: EncodingKey,
    token: Mutex<Option<CachedToken>>,
}

impl FcmPushSender {
    pub fn from_service_account_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path).with_context(|| {
            format!("failed to read Firebase credentials at {}", path.display())
        })?;
        let mut account: ServiceAccount = serde_json::from_str(&raw)
            .context("Firebase service-account credentials are invalid JSON")?;
        if let Ok(project_id) = std::env::var("FIREBASE_PROJECT_ID")
            && !project_id.trim().is_empty()
        {
            account.project_id = project_id;
        }
        if account.project_id.trim().is_empty() || account.client_email.trim().is_empty() {
            bail!("Firebase service-account credentials are missing project identity");
        }
        let encoding_key = EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .context("Firebase service-account private key is invalid")?;
        Ok(Self {
            client: Client::new(),
            project_id: account.project_id,
            client_email: account.client_email,
            token_uri: account.token_uri,
            encoding_key,
            token: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached.as_ref()
            && token.expires_at > Utc::now() + Duration::minutes(5)
        {
            return Ok(token.value.clone());
        }

        let now = Utc::now();
        let claims = TokenClaims {
            iss: &self.client_email,
            scope: FIREBASE_SCOPE,
            aud: &self.token_uri,
            iat: now.timestamp(),
            exp: (now + Duration::hours(1)).timestamp(),
        };
        let assertion = encode(&Header::new(Algorithm::RS256), &claims, &self.encoding_key)
            .context("failed to sign Firebase OAuth assertion")?;
        let response = self
            .client
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .context("Firebase OAuth token request failed")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "Firebase OAuth rejected credentials with HTTP {status}: {}",
                safe_provider_error(&body)
            );
        }
        let response: TokenResponse =
            serde_json::from_str(&body).context("Firebase OAuth returned invalid JSON")?;
        let expires_at = now + Duration::seconds(response.expires_in.max(60));
        let value = response.access_token;
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at,
        });
        Ok(value)
    }
}

#[async_trait]
impl PushSender for FcmPushSender {
    async fn send(&self, message: PushMessage) -> anyhow::Result<DeliveryOutcome> {
        let access_token = self.access_token().await?;
        let mut data = HashMap::from([
            ("category".to_owned(), message.category),
            ("eventType".to_owned(), message.event_type),
        ]);
        if let Some(deep_link) = message.deep_link {
            data.insert("deepLink".to_owned(), deep_link);
        }
        if let Value::Object(values) = message.data {
            for (key, value) in values {
                data.insert(
                    key,
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
        }
        let android_priority = if matches!(message.priority.as_str(), "high" | "urgent") {
            "high"
        } else {
            "normal"
        };
        let payload = json!({
            "message": {
                "token": message.token,
                "notification": {"title": message.title, "body": message.body},
                "data": data,
                "android": {"priority": android_priority},
                "apns": {"payload": {"aps": {"sound": "default"}}}
            }
        });
        let response = self
            .client
            .post(format!(
                "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                self.project_id
            ))
            .bearer_auth(access_token)
            .json(&payload)
            .send()
            .await
            .context("FCM send request failed")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            return Ok(DeliveryOutcome::Sent {
                message_id: parsed["name"].as_str().unwrap_or("accepted").to_owned(),
            });
        }
        if status == StatusCode::NOT_FOUND
            || body.contains("UNREGISTERED")
            || body.contains("registration-token-not-registered")
        {
            return Ok(DeliveryOutcome::InvalidToken);
        }
        bail!(
            "FCM rejected message with HTTP {status}: {}",
            safe_provider_error(&body)
        )
    }

    fn transport(&self) -> &'static str {
        "fcm"
    }
}

pub fn push_from_environment() -> anyhow::Result<Option<Arc<dyn PushSender>>> {
    let enabled = std::env::var("FCM_ENABLED")
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE"))
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .context("GOOGLE_APPLICATION_CREDENTIALS is required when FCM_ENABLED=true")?;
    Ok(Some(Arc::new(FcmPushSender::from_service_account_file(
        Path::new(&path),
    )?)))
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_owned()
}

fn default_expires_in() -> i64 {
    3600
}

fn safe_provider_error(value: &str) -> String {
    value.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_are_bounded() {
        assert_eq!(safe_provider_error(&"x".repeat(800)).len(), 500);
    }

    #[test]
    fn credentials_require_a_valid_private_key() {
        let account = ServiceAccount {
            project_id: "project".into(),
            client_email: "sender@example.test".into(),
            private_key: "not-a-key".into(),
            token_uri: default_token_uri(),
        };
        assert!(EncodingKey::from_rsa_pem(account.private_key.as_bytes()).is_err());
    }

    #[tokio::test]
    #[ignore = "requires GOOGLE_APPLICATION_CREDENTIALS and performs an OAuth request"]
    async fn configured_credentials_can_mint_an_access_token() {
        let path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .expect("GOOGLE_APPLICATION_CREDENTIALS must point to a test credential");
        let sender = FcmPushSender::from_service_account_file(Path::new(&path)).unwrap();
        let token = sender.access_token().await.unwrap();
        assert!(!token.is_empty());
    }
}
