use axum::{Extension, Json, extract::State, http::StatusCode};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::StatusCode as UpstreamStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use uuid::Uuid;

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
    purpose: Option<String>,
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
    purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_balance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_transaction: Option<Value>,
}

pub async fn create_order(
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<CreateOrderResponse>)> {
    let purpose = payment_purpose(request.purpose.as_deref())?;
    require_payment_create(&access, &principal, purpose)?;
    let amount = request
        .amount
        .filter(|amount| *amount >= 100)
        .ok_or_else(|| ApiError::BadRequest("amount must be at least 100 paise".into()))?;
    if purpose == "wallet_top_up" && !(5_000..=500_000).contains(&amount) {
        return Err(ApiError::BadRequest(
            "Wallet top-up must be between 5000 and 500000 paise".into(),
        ));
    }
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
                "purpose": purpose,
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

    let purpose = payment_purpose(order.notes.get("purpose").and_then(Value::as_str))?;
    require_payment_create(&access, &principal, purpose)?;
    let (wallet_balance, wallet_transaction) = if purpose == "wallet_top_up" {
        let result = credit_wallet(&state, &principal, &order, &order_id, &payment_id).await?;
        (Some(result.0), Some(result.1))
    } else {
        record_tuition_payment(&state, &principal, &order, &order_id, &payment_id).await?;
        (None, None)
    };

    Ok(Json(VerifyPaymentResponse {
        success: true,
        order_id,
        payment_id,
        purpose: purpose.into(),
        wallet_balance,
        wallet_transaction,
    }))
}

fn payment_purpose(value: Option<&str>) -> ApiResult<&'static str> {
    match value.unwrap_or("tuition_fee").trim() {
        "tuition_fee" => Ok("tuition_fee"),
        "wallet_top_up" => Ok("wallet_top_up"),
        _ => Err(ApiError::BadRequest("Unsupported payment purpose".into())),
    }
}

fn require_payment_create(
    access: &EffectiveAccess,
    principal: &AuthPrincipal,
    purpose: &str,
) -> ApiResult<()> {
    let allowed = match purpose {
        "wallet_top_up" => {
            principal
                .student
                .portal_families
                .iter()
                .any(|family| family == "student")
                && (access.allows("canteen.wallet.read")
                    || access.allows("canteen.wallet.update")
                    || access.allows("canteen.wallet.top_up"))
        }
        _ => {
            access.allows("tuition_fee.payment.create")
                || access.allows("tuition_fee.payments.create")
        }
    };
    if access.allows("*") || allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn record_tuition_payment(
    state: &AppState,
    principal: &AuthPrincipal,
    order: &RazorpayOrder,
    order_id: &str,
    payment_id: &str,
) -> ApiResult<()> {
    let existing = state
        .list_records(&principal.student.tenant_id, "fees")
        .await?;
    let already_recorded = existing.iter().any(|record| {
        record.record_type == "payments"
            && record.data.get("paymentReference").and_then(Value::as_str) == Some(payment_id)
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
                    "paymentPurpose": "tuition_fee",
                    "paymentReference": payment_id,
                    "razorpayOrderId": order_id,
                    "receipt": order.receipt,
                    "paymentDate": Utc::now(),
                    "status": "verified",
                }),
            )
            .await?;
    }
    Ok(())
}

async fn credit_wallet(
    state: &AppState,
    principal: &AuthPrincipal,
    order: &RazorpayOrder,
    order_id: &str,
    payment_id: &str,
) -> ApiResult<(f64, Value)> {
    let amount = order.amount as f64 / 100.0;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant_id =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM platform.tenants WHERE slug = $1")
            .bind(&principal.student.tenant_id)
            .fetch_optional(database.pool())
            .await?
            .ok_or_else(|| ApiError::NotFound("Tenant not found".into()))?;
    let mut transaction = database.pool().begin().await?;
    let idempotency_key = format!("razorpay:{payment_id}");
    let inserted = sqlx::query(
        r#"INSERT INTO campus_ops.canteen_wallet_transactions
           (tenant_id,user_id,amount,transaction_type,description,reference_id,
            idempotency_key,actor_user_id)
           VALUES($1,$2,$3,'online_top_up','Razorpay wallet top-up',$4,$5,$2)
           ON CONFLICT(tenant_id,idempotency_key) DO NOTHING
           RETURNING id,created_at"#,
    )
    .bind(tenant_id)
    .bind(&principal.student.id)
    .bind(amount)
    .bind(order_id)
    .bind(&idempotency_key)
    .fetch_optional(&mut *transaction)
    .await?;

    let balance = if inserted.is_some() {
        sqlx::query_scalar::<_, f64>(
            r#"INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id,balance,version)
               VALUES($1,$2,$3,1)
               ON CONFLICT(tenant_id,user_id) DO UPDATE SET
                 balance=campus_ops.canteen_wallets.balance+EXCLUDED.balance,
                 version=campus_ops.canteen_wallets.version+1,
                 updated_at=now()
               RETURNING balance::float8"#,
        )
        .bind(tenant_id)
        .bind(&principal.student.id)
        .bind(amount)
        .fetch_one(&mut *transaction)
        .await?
    } else {
        sqlx::query_scalar::<_, f64>(
            "SELECT balance::float8 FROM campus_ops.canteen_wallets WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(tenant_id)
        .bind(&principal.student.id)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(0.0)
    };
    transaction.commit().await?;
    let created_at = inserted
        .as_ref()
        .and_then(|row| row.try_get::<chrono::DateTime<Utc>, _>("created_at").ok())
        .unwrap_or_else(Utc::now);
    let transaction_value = json!({
        "id": inserted
            .as_ref()
            .and_then(|row| row.try_get::<Uuid, _>("id").ok())
            .map(|id| id.to_string())
            .unwrap_or_else(|| payment_id.to_owned()),
        "amount": amount,
        "transactionType": "online_top_up",
        "description": "Razorpay wallet top-up",
        "referenceId": order_id,
        "createdAt": created_at,
    });
    Ok((balance, transaction_value))
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
