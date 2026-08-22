#![forbid(unsafe_code)]

//! SuperCampus authn platform capability.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;
const JWT_HEADER: &str = r#"{"alg":"HS256","typ":"JWT"}"#;
const JWT_LEEWAY_SECONDS: usize = 30;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JwtHeader {
    alg: String,
    typ: String,
}

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("token encoding failed")]
    Encoding(#[source] serde_json::Error),
    #[error("token is malformed")]
    Malformed,
    #[error("token header is not supported")]
    UnsupportedHeader,
    #[error("token signature is invalid")]
    InvalidSignature,
    #[error("token claims are invalid")]
    InvalidClaims,
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
    TokenCreation(#[source] JwtError),
    #[error("access token has expired")]
    ExpiredToken(#[source] JwtError),
    #[error("access token is invalid")]
    InvalidToken(#[source] JwtError),
}

#[derive(Clone)]
pub struct AuthService {
    issuer: String,
    audience: String,
    secret: Vec<u8>,
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
            secret: config.secret.into_bytes(),
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
        self.issue_access_token_with_ttl(
            user_id,
            tenant_id,
            session_id,
            roles,
            self.access_token_ttl_seconds,
        )
    }

    /// Issues an access token with an explicit lifetime.
    ///
    /// Used for the realtime WebSocket handshake, which needs a token that expires in
    /// seconds rather than minutes because it travels in a URL.
    pub fn issue_access_token_with_ttl(
        &self,
        user_id: &str,
        tenant_id: &str,
        session_id: Uuid,
        roles: Vec<String>,
        ttl_seconds: i64,
    ) -> Result<IssuedAccessToken, AuthError> {
        if ttl_seconds <= 0 {
            return Err(AuthError::InvalidLifetime);
        }
        let issued_at = Utc::now();
        let expires_at = issued_at + Duration::seconds(ttl_seconds);
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
        let token = self
            .encode_access_token(&claims)
            .map_err(AuthError::TokenCreation)?;
        Ok(IssuedAccessToken { token, expires_at })
    }

    pub fn verify_access_token(&self, token: &str) -> Result<AccessClaims, AuthError> {
        self.decode_access_token(token)
    }

    fn encode_access_token(&self, claims: &AccessClaims) -> Result<String, JwtError> {
        let header = URL_SAFE_NO_PAD.encode(JWT_HEADER);
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(JwtError::Encoding)?);
        let signing_input = format!("{header}.{payload}");
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC accepts keys of any non-zero length");
        mac.update(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        Ok(format!("{signing_input}.{signature}"))
    }

    fn decode_access_token(&self, token: &str) -> Result<AccessClaims, AuthError> {
        if token.is_empty() || token.len() > MAX_ACCESS_TOKEN_BYTES {
            return Err(AuthError::InvalidToken(JwtError::Malformed));
        }
        let mut segments = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) else {
            return Err(AuthError::InvalidToken(JwtError::Malformed));
        };

        let decoded_header = URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| AuthError::InvalidToken(JwtError::Malformed))?;
        let header_value: JwtHeader = serde_json::from_slice(&decoded_header)
            .map_err(|_| AuthError::InvalidToken(JwtError::Malformed))?;
        if header_value.alg != "HS256" || header_value.typ != "JWT" {
            return Err(AuthError::InvalidToken(JwtError::UnsupportedHeader));
        }

        let decoded_signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| AuthError::InvalidToken(JwtError::Malformed))?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC accepts keys of any non-zero length");
        mac.update(format!("{header}.{payload}").as_bytes());
        mac.verify_slice(&decoded_signature)
            .map_err(|_| AuthError::InvalidToken(JwtError::InvalidSignature))?;

        let decoded_payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| AuthError::InvalidToken(JwtError::Malformed))?;
        let claims: AccessClaims = serde_json::from_slice(&decoded_payload)
            .map_err(|_| AuthError::InvalidToken(JwtError::Malformed))?;
        let now =
            timestamp(Utc::now()).map_err(|_| AuthError::InvalidToken(JwtError::InvalidClaims))?;
        if now > claims.exp.saturating_add(JWT_LEEWAY_SECONDS) {
            return Err(AuthError::ExpiredToken(JwtError::InvalidClaims));
        }
        let latest_acceptable = now.saturating_add(JWT_LEEWAY_SECONDS);
        if claims.sub.trim().is_empty()
            || claims.tid.trim().is_empty()
            || claims.iss != self.issuer
            || claims.aud != self.audience
            || claims.iat > latest_acceptable
            || claims.nbf > latest_acceptable
            || claims.exp < claims.iat
        {
            return Err(AuthError::InvalidToken(JwtError::InvalidClaims));
        }
        Ok(claims)
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

    #[test]
    fn expired_tokens_are_distinct_from_malformed_tokens() {
        let service = AuthService::default();
        let now = Utc::now();
        let claims = AccessClaims {
            sub: "student-1".into(),
            tid: "tenant-a".into(),
            sid: Uuid::new_v4(),
            roles: vec!["student".into()],
            iss: service.issuer.clone(),
            aud: service.audience.clone(),
            iat: timestamp(now - Duration::minutes(2)).unwrap(),
            nbf: timestamp(now - Duration::minutes(2)).unwrap(),
            exp: timestamp(now - Duration::minutes(1)).unwrap(),
            jti: Uuid::new_v4(),
        };
        let token = service.encode_access_token(&claims).unwrap();

        assert!(matches!(
            service.verify_access_token(&token),
            Err(AuthError::ExpiredToken(_))
        ));
        assert!(matches!(
            service.verify_access_token("not-a-jwt"),
            Err(AuthError::InvalidToken(_))
        ));
    }

    #[test]
    fn rejects_tampered_tokens_and_algorithm_confusion() {
        let service = AuthService::default();
        let issued = service
            .issue_access_token(
                "student-1",
                "tenant-a",
                Uuid::new_v4(),
                vec!["student".into()],
            )
            .unwrap();
        let mut tampered = issued.token.into_bytes();
        let payload_start = tampered.iter().position(|byte| *byte == b'.').unwrap() + 1;
        tampered[payload_start] = if tampered[payload_start] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(matches!(
            service.verify_access_token(&tampered),
            Err(AuthError::InvalidToken(_))
        ));

        let unsupported_header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(b"{}");
        let token = format!("{unsupported_header}.{payload}.");
        assert!(matches!(
            service.verify_access_token(&token),
            Err(AuthError::InvalidToken(JwtError::UnsupportedHeader))
        ));
    }

    #[test]
    fn rejects_wrong_issuer_audience_and_future_tokens() {
        let service = AuthService::default();
        let now = Utc::now();
        let valid_claims = AccessClaims {
            sub: "student-1".into(),
            tid: "tenant-a".into(),
            sid: Uuid::new_v4(),
            roles: vec!["student".into()],
            iss: service.issuer.clone(),
            aud: service.audience.clone(),
            iat: timestamp(now).unwrap(),
            nbf: timestamp(now).unwrap(),
            exp: timestamp(now + Duration::minutes(15)).unwrap(),
            jti: Uuid::new_v4(),
        };

        for claims in [
            AccessClaims {
                iss: "https://attacker.invalid".into(),
                ..valid_claims.clone()
            },
            AccessClaims {
                aud: "another-api".into(),
                ..valid_claims.clone()
            },
            AccessClaims {
                iat: timestamp(now + Duration::minutes(5)).unwrap(),
                nbf: timestamp(now + Duration::minutes(5)).unwrap(),
                exp: timestamp(now + Duration::minutes(20)).unwrap(),
                ..valid_claims
            },
        ] {
            let token = service.encode_access_token(&claims).unwrap();
            assert!(matches!(
                service.verify_access_token(&token),
                Err(AuthError::InvalidToken(JwtError::InvalidClaims))
            ));
        }
    }
}
