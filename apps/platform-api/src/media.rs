use axum::extract::Multipart;
use chrono::Utc;
use sha1::{Digest, Sha1};
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};

const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

pub async fn upload_to_cloudinary(mut multipart: Multipart, tenant_id: &str) -> ApiResult<Value> {
    let (cloud_name, api_key, api_secret) = cloudinary_credentials()?;
    let mut bytes = None;
    let mut filename = "upload".to_string();
    let mut content_type = "application/octet-stream".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|_| ApiError::BadRequest("Invalid upload payload".into()))? {
        if field.name() != Some("file") {
            continue;
        }
        if let Some(name) = field.file_name() {
            filename = name.to_string();
        }
        if let Some(mime) = field.content_type() {
            content_type = mime.to_string();
        }
        let data = field.bytes().await.map_err(|_| ApiError::BadRequest("Unable to read uploaded file".into()))?;
        if data.len() > MAX_UPLOAD_BYTES {
            return Err(ApiError::BadRequest("File must be 10 MB or smaller".into()));
        }
        if !content_type.starts_with("image/") && !content_type.starts_with("application/pdf") {
            return Err(ApiError::BadRequest("Only images and PDF files are supported".into()));
        }
        bytes = Some(data);
        break;
    }

    let bytes = bytes.ok_or_else(|| ApiError::BadRequest("A file is required".into()))?;
    let timestamp = Utc::now().timestamp();
    let folder = format!("supercampus/{tenant_id}/media");
    let signature_base = format!("folder={folder}&timestamp={timestamp}{api_secret}");
    let mut hasher = Sha1::new();
    hasher.update(signature_base.as_bytes());
    let signature = format!("{:x}", hasher.finalize());
    let endpoint = format!("https://api.cloudinary.com/v1_1/{cloud_name}/auto/upload");
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename)
        .mime_str(&content_type)
        .map_err(|_| ApiError::BadRequest("Unsupported file type".into()))?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("api_key", api_key)
        .text("timestamp", timestamp.to_string())
        .text("signature", signature)
        .text("folder", folder);
    let response = reqwest::Client::new()
        .post(endpoint)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            tracing::error!(?error, "cloudinary upload request failed");
            ApiError::Internal
        })?;
    let status = response.status();
    let payload: Value = response.json().await.map_err(|_| ApiError::Internal)?;
    if !status.is_success() {
        tracing::error!(%status, "cloudinary rejected upload");
        return Err(ApiError::BadRequest(payload.get("error").and_then(|error| error.get("message")).and_then(Value::as_str).unwrap_or("Cloudinary rejected the upload").to_string()));
    }
    Ok(json!({
        "secureUrl": payload.get("secure_url").and_then(Value::as_str),
        "publicId": payload.get("public_id").and_then(Value::as_str),
        "resourceType": payload.get("resource_type").and_then(Value::as_str),
        "bytes": payload.get("bytes").and_then(Value::as_u64),
    }))
}

fn cloudinary_credentials() -> ApiResult<(String, String, String)> {
    if let (Ok(cloud), Ok(key), Ok(secret)) = (
        std::env::var("CLOUDINARY_CLOUD_NAME"),
        std::env::var("CLOUDINARY_API_KEY"),
        std::env::var("CLOUDINARY_API_SECRET"),
    ) {
        return Ok((cloud, key, secret));
    }
    let value = std::env::var("CLOUDINARY_URL").map_err(|_| ApiError::Internal)?;
    let value = value.strip_prefix("cloudinary://").ok_or(ApiError::Internal)?;
    let (credentials, cloud_name) = value.rsplit_once('@').ok_or(ApiError::Internal)?;
    let (api_key, api_secret) = credentials.split_once(':').ok_or(ApiError::Internal)?;
    Ok((cloud_name.to_string(), api_key.to_string(), api_secret.to_string()))
}
