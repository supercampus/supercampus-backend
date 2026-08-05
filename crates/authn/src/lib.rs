#![forbid(unsafe_code)]

//! SuperCampus authn platform capability.

use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CRATE_NAME: &str = "supercampus-authn";
pub const DEFAULT_ACCESS_TOKEN_TTL_SECONDS: i64 = 15 * 60;
pub const DEFAULT_REFRESH_TOKEN_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessClaims {
    pub sub: String,
    pub tid: String,
    pub sid: Uuid,
    pub roles: Vec<String>,
    pub iss: String,
    pub aud: String,
    pub iat: usize,
    pub nbf: usize,
    pub exp: usize,
    pub jti: Uuid,
}

#[derive(Debug, Clone)]
pub struct IssuedAccessToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    pub secret: String,
    pub access_token_ttl_seconds: i64,
    pub refresh_token_ttl_seconds: i64,
}

impl AuthConfig {
    pub fn development() -> Self {
        Self {
            issuer: "https://auth.supercampus.local".into(),
            audience: "supercampus-api".into(),
            secret: "supercampus-local-test-secret-change-before-production-2026".into(),
            access_token_ttl_seconds: DEFAULT_ACCESS_TOKEN_TTL_SECONDS,
            refresh_token_ttl_seconds: DEFAULT_REFRESH_TOKEN_TTL_SECONDS,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("JWT secret must contain at least 32 bytes")]
    WeakSecret,
    #[error("token lifetime must be greater than zero")]
    InvalidLifetime,
    #[error("JWT timestamp is outside the supported range")]
    InvalidTimestamp,
    #[error("failed to create access token")]
    TokenCreation(#[source] jsonwebtoken::errors::Error),
    #[error("access token is invalid or expired")]
    InvalidToken(#[source] jsonwebtoken::errors::Error),
}

#[derive(Clone)]
pub struct AuthService {
    issuer: String,
    audience: String,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    access_token_ttl_seconds: i64,
    refresh_token_ttl_seconds: i64,
}

impl AuthService {
    pub fn new(config: AuthConfig) -> Result<Self, AuthError> {
        if config.secret.len() < 32 {
            return Err(AuthError::WeakSecret);
        }
        if config.access_token_ttl_seconds <= 0 || config.refresh_token_ttl_seconds <= 0 {
            return Err(AuthError::InvalidLifetime);
        }
        Ok(Self {
            issuer: config.issuer,
            audience: config.audience,
            encoding_key: EncodingKey::from_secret(config.secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(config.secret.as_bytes()),
            access_token_ttl_seconds: config.access_token_ttl_seconds,
            refresh_token_ttl_seconds: config.refresh_token_ttl_seconds,
        })
    }

    pub fn issue_access_token(
        &self,
        user_id: &str,
        tenant_id: &str,
        session_id: Uuid,
        roles: Vec<String>,
    ) -> Result<IssuedAccessToken, AuthError> {
        let issued_at = Utc::now();
        let expires_at = issued_at + Duration::seconds(self.access_token_ttl_seconds);
        let claims = AccessClaims {
            sub: user_id.to_owned(),
            tid: tenant_id.to_owned(),
            sid: session_id,
            roles,
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
            iat: timestamp(issued_at)?,
            nbf: timestamp(issued_at)?,
            exp: timestamp(expires_at)?,
            jti: Uuid::new_v4(),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .map_err(AuthError::TokenCreation)?;
        Ok(IssuedAccessToken { token, expires_at })
    }

    pub fn verify_access_token(&self, token: &str) -> Result<AccessClaims, AuthError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "sub", "iss", "aud"]);
        validation.leeway = 30;
        decode::<AccessClaims>(token, &self.decoding_key, &validation)
            .map(|token| token.claims)
            .map_err(AuthError::InvalidToken)
    }

    pub fn refresh_expires_at(&self) -> DateTime<Utc> {
        Utc::now() + Duration::seconds(self.refresh_token_ttl_seconds)
    }

    pub fn access_token_ttl_seconds(&self) -> i64 {
        self.access_token_ttl_seconds
    }

    pub fn refresh_token_ttl_seconds(&self) -> i64 {
        self.refresh_token_ttl_seconds
    }
}

impl Default for AuthService {
    fn default() -> Self {
        Self::new(AuthConfig::development()).expect("development auth config is valid")
    }
}

pub fn generate_refresh_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn hash_refresh_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn timestamp(value: DateTime<Utc>) -> Result<usize, AuthError> {
    usize::try_from(value.timestamp()).map_err(|_| AuthError::InvalidTimestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_round_trip_preserves_tenant_session_and_roles() {
        let service = AuthService::default();
        let session_id = Uuid::new_v4();
        let issued = service
            .issue_access_token("student-1", "tenant-a", session_id, vec!["student".into()])
            .unwrap();
        let claims = service.verify_access_token(&issued.token).unwrap();
        assert_eq!(claims.sub, "student-1");
        assert_eq!(claims.tid, "tenant-a");
        assert_eq!(claims.sid, session_id);
        assert_eq!(claims.roles, ["student"]);
    }

    #[test]
    fn refresh_tokens_are_random_and_only_stable_after_hashing() {
        let first = generate_refresh_token();
        let second = generate_refresh_token();
        assert_ne!(first, second);
        assert_eq!(hash_refresh_token(&first), hash_refresh_token(&first));
        assert_ne!(hash_refresh_token(&first), hash_refresh_token(&second));
    }
}
