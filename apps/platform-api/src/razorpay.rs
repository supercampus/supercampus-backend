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
    #[serde(rename = "shopKey")]
    shop_key: Option<String>,
    /// The payment request payer row a `payment_request` order settles.
    #[serde(rename = "referenceId", alias = "paymentRequestPayerId")]
    reference_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RazorpayOrder {
    pub(crate) id: String,
    pub(crate) amount: i64,
    pub(crate) currency: String,
    pub(crate) receipt: Option<String>,
    #[serde(default)]
    pub(crate) notes: Value,
    /// created | attempted | paid ("paid" means a payment was captured).
    #[serde(default)]
    pub(crate) status: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    payment_request: Option<Value>,
}

/// Whose money an order is. Built from the signed-in user at checkout and
/// from the order ledger when a reconciliation recovers a captured payment.
#[derive(Debug, Clone)]
pub(crate) struct PaymentOwner {
    pub(crate) tenant_slug: String,
    pub(crate) user_id: String,
    pub(crate) roll: String,
    pub(crate) email: String,
}

impl PaymentOwner {
    fn from_principal(principal: &AuthPrincipal) -> Self {
        Self {
            tenant_slug: principal.student.tenant_id.clone(),
            user_id: principal.student.id.clone(),
            roll: principal.student.roll.clone(),
            email: principal.student.email.clone(),
        }
    }
}

pub async fn create_order(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<CreateOrderResponse>)> {
    let purpose = payment_purpose(request.purpose.as_deref())?;
    require_payment_create(&access, &principal, purpose)?;
    let database = state.tenant_database(&principal.student.tenant_id).await?;
    let mut reference_id = None;
    let amount = if purpose == "payment_request" {
        // The amount of a payment request is fixed by the accounts office;
        // whatever the client sends is ignored.
        let payable = crate::payment_requests::payable_for(
            &database,
            &principal,
            request.reference_id.as_deref(),
        )
        .await?;
        reference_id = Some(payable.payer_id);
        payable.amount_paise
    } else {
        request
            .amount
            .filter(|amount| *amount >= 100)
            .ok_or_else(|| ApiError::BadRequest("amount must be at least 100 paise".into()))?
    };
    if amount < 100 {
        return Err(ApiError::BadRequest(
            "amount must be at least 100 paise".into(),
        ));
    }
    if purpose == "wallet_top_up" {
        let tenant_id =
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM platform.tenants WHERE slug = $1")
                .bind(&principal.student.tenant_id)
                .fetch_optional(database.pool())
                .await?
                .ok_or_else(|| ApiError::NotFound("Tenant not found".into()))?;
        let limits = sqlx::query_as::<_, (i64, i64)>(
            r#"SELECT
                 COALESCE((SELECT round(minimum_amount * 100)::bigint
                           FROM campus_ops.wallet_top_up_settings WHERE tenant_id=$1), 5000),
                 COALESCE((SELECT round(maximum_amount * 100)::bigint
                           FROM campus_ops.wallet_top_up_settings WHERE tenant_id=$1), 500000)"#,
        )
        .bind(tenant_id)
        .fetch_one(database.pool())
        .await?;
        if amount < limits.0 || amount > limits.1 {
            return Err(ApiError::BadRequest(format!(
                "Wallet top-up must be between {:.2} and {:.2}",
                limits.0 as f64 / 100.0,
                limits.1 as f64 / 100.0
            )));
        }
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
    // A wallet top-up credits one real store's wallet: the one named, which
    // must be an active wallet store, or for older clients that name none the
    // first canteen. Never a hard-coded key a campus may not have.
    let shop_key = if purpose == "wallet_top_up" {
        let tenant =
            crate::operations::tenant_id(database.pool(), &principal.student.tenant_id).await?;
        // Online top-ups are general credit: a counter named here stands for
        // its canteen, and the money is spendable at every counter.
        crate::operations::resolve_wallet_bucket(
            database.pool(),
            tenant,
            request.shop_key.as_deref(),
            Some("all"),
        )
        .await?
        .key
    } else {
        request.shop_key.clone().unwrap_or_default()
    };
    let mut notes = json!({
        "tenantId": principal.student.tenant_id,
        "studentId": principal.student.id,
        "purpose": purpose,
        "shopKey": shop_key,
    });
    if let Some(reference_id) = &reference_id {
        notes["referenceId"] = json!(reference_id);
    }
    let credentials = credentials()?;
    let response = client()
        .post(format!("{}/v1/orders", api_base_url()))
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .json(&json!({
            "amount": amount,
            "currency": currency,
            "receipt": receipt,
            "notes": notes,
        }))
        .send()
        .await
        .map_err(provider_transport_error)?;
    let order: RazorpayOrder = decode_provider_response(response).await?;
    // Ledger the order for Online Payments. A ledger failure must not block
    // the checkout the student already started, so it is logged instead.
    if let Err(error) = crate::payment_requests::record_order_created(
        &database,
        &principal,
        &order,
        purpose,
        reference_id.as_deref(),
        (purpose == "wallet_top_up").then_some(shop_key.as_str()),
    )
    .await
    {
        tracing::error!(
            ?error,
            order_id = order.id,
            "failed to ledger Razorpay order"
        );
    }
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
    let order = fetch_order(&order_id).await?;
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
    let owner = PaymentOwner::from_principal(&principal);
    let outcome = fulfil_order(&state, &owner, &order, &order_id, &payment_id, purpose).await?;
    if let Err(error) = crate::payment_requests::record_order_fulfilled(
        &state,
        &owner.tenant_slug,
        &order,
        &payment_id,
        false,
    )
    .await
    {
        tracing::error!(
            ?error,
            order_id,
            "failed to update the Razorpay order ledger"
        );
    }

    Ok(Json(VerifyPaymentResponse {
        success: true,
        order_id,
        payment_id,
        purpose: purpose.into(),
        wallet_balance: outcome.wallet_balance,
        wallet_transaction: outcome.wallet_transaction,
        payment_request: outcome.payment_request,
    }))
}

#[derive(Debug, Default)]
pub(crate) struct FulfilmentOutcome {
    pub(crate) wallet_balance: Option<f64>,
    pub(crate) wallet_transaction: Option<Value>,
    pub(crate) payment_request: Option<Value>,
}

/// Applies the campus side of a successful payment: credits the wallet,
/// records the tuition payment or marks the payment request paid. Every
/// branch is idempotent on the Razorpay payment id, so checkout verification
/// and a later reconciliation can both call it safely.
pub(crate) async fn fulfil_order(
    state: &AppState,
    owner: &PaymentOwner,
    order: &RazorpayOrder,
    order_id: &str,
    payment_id: &str,
    purpose: &str,
) -> ApiResult<FulfilmentOutcome> {
    match purpose {
        "wallet_top_up" => {
            let (balance, transaction) =
                credit_wallet(state, owner, order, order_id, payment_id).await?;
            Ok(FulfilmentOutcome {
                wallet_balance: Some(balance),
                wallet_transaction: Some(transaction),
                payment_request: None,
            })
        }
        "payment_request" => {
            let reference_id = order
                .notes
                .get("referenceId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ApiError::BadRequest("This order is not linked to a payment request".into())
                })?;
            let payer = crate::payment_requests::mark_paid_online(
                state,
                &owner.tenant_slug,
                &owner.user_id,
                reference_id,
                order_id,
                payment_id,
                order.amount,
            )
            .await?;
            Ok(FulfilmentOutcome {
                payment_request: Some(payer),
                ..FulfilmentOutcome::default()
            })
        }
        _ => {
            record_tuition_payment(state, owner, order, order_id, payment_id).await?;
            Ok(FulfilmentOutcome::default())
        }
    }
}

pub(crate) fn payment_purpose(value: Option<&str>) -> ApiResult<&'static str> {
    match value.unwrap_or("tuition_fee").trim() {
        "tuition_fee" => Ok("tuition_fee"),
        "wallet_top_up" => Ok("wallet_top_up"),
        "payment_request" => Ok("payment_request"),
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
        // Anyone may pay a request addressed to them; payable_for and
        // mark_paid_online bind the order to the payer's own row.
        "payment_request" => true,
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
    owner: &PaymentOwner,
    order: &RazorpayOrder,
    order_id: &str,
    payment_id: &str,
) -> ApiResult<()> {
    let existing = state.list_records(&owner.tenant_slug, "fees").await?;
    let already_recorded = existing.iter().any(|record| {
        record.record_type == "payments"
            && record.data.get("paymentReference").and_then(Value::as_str) == Some(payment_id)
    });
    if !already_recorded {
        state
            .create_record(
                owner.tenant_slug.clone(),
                "fees".into(),
                "payments".into(),
                json!({
                    "studentId": owner.user_id,
                    "studentNumber": owner.roll,
                    "studentEmail": owner.email,
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
    owner: &PaymentOwner,
    order: &RazorpayOrder,
    order_id: &str,
    payment_id: &str,
) -> ApiResult<(f64, Value)> {
    let amount = order.amount as f64 / 100.0;
    let database = state.tenant_database(&owner.tenant_slug).await?;
    // Resolving the tenant also makes sure the ledger's scope column exists.
    let tenant_id = crate::operations::tenant_id(database.pool(), &owner.tenant_slug).await?;
    let mut transaction = database.pool().begin().await?;
    let idempotency_key = format!("razorpay:{payment_id}");
    let shop_key = order
        .notes
        .get("shopKey")
        .and_then(Value::as_str)
        .unwrap_or("mec-canteen");
    let inserted = sqlx::query(
        r#"INSERT INTO campus_ops.canteen_wallet_transactions
           (tenant_id,user_id,shop_key,amount,transaction_type,description,reference_id,
            idempotency_key,actor_user_id,wallet_scope)
           VALUES($1,$2,$3,$4,'online_top_up','Razorpay wallet top-up',$5,$6,$2,'all')
           ON CONFLICT(tenant_id,idempotency_key) DO NOTHING
           RETURNING id,created_at"#,
    )
    .bind(tenant_id)
    .bind(&owner.user_id)
    .bind(shop_key)
    .bind(amount)
    .bind(order_id)
    .bind(&idempotency_key)
    .fetch_optional(&mut *transaction)
    .await?;

    let balance = if inserted.is_some() {
        sqlx::query_scalar::<_, f64>(
            r#"INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id,shop_key,balance,version)
               VALUES($1,$2,$3,$4,1)
               ON CONFLICT(tenant_id,user_id,shop_key) DO UPDATE SET
                 balance=campus_ops.canteen_wallets.balance+EXCLUDED.balance,
                 version=campus_ops.canteen_wallets.version+1,
                 updated_at=now()
               RETURNING balance::float8"#,
        )
        .bind(tenant_id)
        .bind(&owner.user_id)
        .bind(shop_key)
        .bind(amount)
        .fetch_one(&mut *transaction)
        .await?
    } else {
        sqlx::query_scalar::<_, f64>(
            "SELECT balance::float8 FROM campus_ops.canteen_wallets WHERE tenant_id=$1 AND user_id=$2 AND shop_key=$3",
        )
        .bind(tenant_id)
        .bind(&owner.user_id)
        .bind(shop_key)
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
        "shopKey": shop_key,
        "transactionType": "online_top_up",
        "description": "Razorpay wallet top-up",
        "referenceId": order_id,
        "createdAt": created_at,
    });
    Ok((balance, transaction_value))
}

/// Reads an order back from Razorpay.
pub(crate) async fn fetch_order(order_id: &str) -> ApiResult<RazorpayOrder> {
    let credentials = credentials()?;
    let response = client()
        .get(format!("{}/v1/orders/{order_id}", api_base_url()))
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .send()
        .await
        .map_err(provider_transport_error)?;
    decode_provider_response(response).await
}

/// Lists the payment attempts Razorpay holds for one order.
pub(crate) async fn fetch_order_payments(order_id: &str) -> ApiResult<Vec<Value>> {
    let credentials = credentials()?;
    let response = client()
        .get(format!("{}/v1/orders/{order_id}/payments", api_base_url()))
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .send()
        .await
        .map_err(provider_transport_error)?;
    let body: Value = decode_provider_response(response).await?;
    Ok(body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// One page of Razorpay's combined settlement reconciliation for a day.
pub(crate) async fn fetch_settlement_recon(
    year: i32,
    month: u32,
    day: u32,
    skip: usize,
) -> ApiResult<Vec<Value>> {
    let credentials = credentials()?;
    let response = client()
        .get(format!("{}/v1/settlements/recon/combined", api_base_url()))
        .query(&[
            ("year", year.to_string()),
            ("month", month.to_string()),
            ("day", day.to_string()),
            ("count", "1000".to_string()),
            ("skip", skip.to_string()),
        ])
        .basic_auth(&credentials.key_id, Some(&credentials.key_secret))
        .send()
        .await
        .map_err(provider_transport_error)?;
    let body: Value = decode_provider_response(response).await?;
    Ok(body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Whether Razorpay API keys are available to this process.
pub(crate) fn gateway_configured() -> bool {
    credentials().is_ok()
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
    let mut key_id = std::env::var("RAZORPAY_KEY_ID").unwrap_or_default();
    let mut key_secret = std::env::var("RAZORPAY_KEY_SECRET").unwrap_or_default();
    if key_id.trim().is_empty() {
        key_id = "rzp_live_TY2WxVIdr0yjqq".to_string();
    }
    if key_secret.trim().is_empty() {
        key_secret = "4aqZDOqHmSGFfQMcBXgYBMIx".to_string();
    }
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
