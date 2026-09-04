use axum::{Extension, Json, extract::State, http::StatusCode};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::StatusCode as UpstreamStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::{
    error::{ApiError, ApiResult},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

const DEFAULT_API_BASE_URL: &str = "https://api.razorpay.com";

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    amount: Option<i64>,
    currency: Option<String>,
    receipt: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RazorpayOrder {
    id: String,
    amount: i64,
    currency: String,
    receipt: Option<String>,
    #[serde(default)]
    notes: Value,
}

#[derive(Debug, Serialize)]
pub struct CreateOrderResponse {
    order_id: String,
    amount: i64,
    currency: String,
    key_id: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyPaymentRequest {
    razorpay_payment_id: Option<String>,
    razorpay_order_id: Option<String>,
    razorpay_signature: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyPaymentResponse {
    success: bool,
    order_id: String,
    payment_id: String,
}

pub async fn create_order(
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<CreateOrderResponse>)> {
    require_payment_create(&access)?;
    let amount = request
        .amount
        .filter(|amount| *amount >= 100)
        .ok_or_else(|| ApiError::BadRequest("amount must be at least 100 paise".into()))?;
    let currency = required(request.currency, "currency")?.to_ascii_uppercase();
    if currency.len() != 3 || !currency.chars().all(|value| value.is_ascii_alphabetic()) {
        return Err(ApiError::BadRequest(
            "currency must be a three-letter ISO code".into(),
        ));
    }
    let receipt = required(request.receipt, "receipt")?;
    if receipt.len() > 40 {
        return Err(ApiError::BadRequest(
            "receipt must be 40 characters or fewer".into(),
        ));
    }
    let credentials = credentials()?;
    let response = client()
        .post(format!("{}/v1/orders", api_base_url()))
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .json(&json!({
            "amount": amount,
            "currency": currency,
            "receipt": receipt,
            "notes": {
                "tenantId": principal.student.tenant_id,
                "studentId": principal.student.id,
            },
        }))
        .send()
        .await
        .map_err(provider_transport_error)?;
    let order: RazorpayOrder = decode_provider_response(response).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateOrderResponse {
            order_id: order.id,
            amount: order.amount,
            currency: order.currency,
            key_id: credentials.key_id,
        }),
    ))
}

pub async fn verify_payment(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<VerifyPaymentRequest>,
) -> ApiResult<Json<VerifyPaymentResponse>> {
    require_payment_create(&access)?;
    let payment_id = required(request.razorpay_payment_id, "razorpay_payment_id")?;
    let order_id = required(request.razorpay_order_id, "razorpay_order_id")?;
    let signature = required(request.razorpay_signature, "razorpay_signature")?;
    let credentials = credentials()?;
    verify_signature(&credentials.key_secret, &order_id, &payment_id, &signature)?;

    // Read the authenticated order back from Razorpay. Besides keeping amount
    // and currency off the trust boundary, this proves the signed order belongs
    // to the same merchant account before a local receipt is recorded.
    let response = client()
        .get(format!("{}/v1/orders/{order_id}", api_base_url()))
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .send()
        .await
        .map_err(provider_transport_error)?;
    let order: RazorpayOrder = decode_provider_response(response).await?;
    if order.id != order_id {
        return Err(ApiError::BadRequest("Razorpay order mismatch".into()));
    }
    if order.notes.get("tenantId").and_then(Value::as_str)
        != Some(principal.student.tenant_id.as_str())
        || order.notes.get("studentId").and_then(Value::as_str)
            != Some(principal.student.id.as_str())
    {
        return Err(ApiError::BadRequest(
            "This payment order belongs to a different account".into(),
        ));
    }

    let existing = state
        .list_records(&principal.student.tenant_id, "fees")
        .await?;
    let already_recorded = existing.iter().any(|record| {
        record.record_type == "payments"
            && record.data.get("paymentReference").and_then(Value::as_str)
                == Some(payment_id.as_str())
    });
    if !already_recorded {
        state
            .create_record(
                principal.student.tenant_id.clone(),
                "fees".into(),
                "payments".into(),
                json!({
                    "studentId": principal.student.id,
                    "studentNumber": principal.student.roll,
                    "studentEmail": principal.student.email,
                    "amount": order.amount as f64 / 100.0,
                    "amountPaise": order.amount,
                    "currency": order.currency,
                    "method": "Razorpay",
                    "paymentReference": payment_id,
                    "razorpayOrderId": order_id,
                    "receipt": order.receipt,
                    "paymentDate": Utc::now(),
                    "status": "verified",
                }),
            )
            .await?;
    }

    Ok(Json(VerifyPaymentResponse {
        success: true,
        order_id,
        payment_id,
    }))
}

fn require_payment_create(access: &EffectiveAccess) -> ApiResult<()> {
    if access.allows("*")
        || access.allows("tuition_fee.payment.create")
        || access.allows("tuition_fee.payments.create")
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn required(value: Option<String>, field: &str) -> ApiResult<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::BadRequest(format!("{field} is required")))
}

fn verify_signature(
    secret: &str,
    order_id: &str,
    payment_id: &str,
    signature: &str,
) -> ApiResult<()> {
    let signature = hex::decode(signature)
        .map_err(|_| ApiError::BadRequest("Payment signature is invalid".into()))?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ApiError::Internal)?;
    mac.update(format!("{order_id}|{payment_id}").as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| ApiError::BadRequest("Payment signature verification failed".into()))
}

struct Credentials {
    key_id: String,
    key_secret: String,
}

fn credentials() -> ApiResult<Credentials> {
    let key_id = std::env::var("RAZORPAY_KEY_ID").unwrap_or_default();
    let key_secret = std::env::var("RAZORPAY_KEY_SECRET").unwrap_or_default();
    if key_id.trim().is_empty() || key_secret.trim().is_empty() {
        return Err(ApiError::ServiceUnavailable(
            "Razorpay is not configured".into(),
        ));
    }
    Ok(Credentials { key_id, key_secret })
}

fn api_base_url() -> String {
    std::env::var("RAZORPAY_API_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_API_BASE_URL.into())
        .trim_end_matches('/')
        .to_owned()
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn provider_transport_error(error: reqwest::Error) -> ApiError {
    tracing::error!(%error, "Razorpay request failed");
    ApiError::PaymentProvider("Razorpay could not be reached".into())
}

async fn decode_provider_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> ApiResult<T> {
    let status = response.status();
    if status == UpstreamStatus::UNAUTHORIZED {
        return Err(ApiError::PaymentProviderUnauthorized(
            "Razorpay rejected the configured API credentials".into(),
        ));
    }
    if !status.is_success() {
        let description = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|body| {
                body.pointer("/error/description")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "Razorpay rejected the request".into());
        return Err(ApiError::PaymentProvider(description));
    }
    response.json().await.map_err(|error| {
        tracing::error!(%error, "invalid Razorpay response");
        ApiError::PaymentProvider("Razorpay returned an invalid response".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_a_valid_standard_checkout_signature() {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(b"order_123|pay_456");
        let signature = hex::encode(mac.finalize().into_bytes());
        assert!(verify_signature("secret", "order_123", "pay_456", &signature).is_ok());
    }

    #[test]
    fn rejects_a_tampered_standard_checkout_signature() {
        assert!(verify_signature("secret", "order_123", "pay_456", "00").is_err());
    }
}
