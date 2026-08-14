//! Authenticated tenant media uploads.
//!
//! Cloudinary credentials never cross the API boundary. The server validates
//! the file bytes, creates a tenant-scoped signed request, and returns only the
//! persisted asset reference needed by the frontend.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use axum::extract::Multipart;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::error::{ApiError, ApiResult};

pub const MAX_MEDIA_BYTES: usize = 10 * 1024 * 1024;
pub const MULTIPART_BODY_LIMIT: usize = MAX_MEDIA_BYTES + 256 * 1024;

#[derive(Debug, Clone)]
struct CloudinaryConfig {
    cloud_name: String,
    api_key: String,
    api_secret: String,
}

impl CloudinaryConfig {
    fn from_environment() -> anyhow::Result<Self> {
        Ok(Self {
            cloud_name: required_environment("CLOUDINARY_CLOUD_NAME")?,
            api_key: required_environment("CLOUDINARY_API_KEY")?,
            api_secret: required_environment("CLOUDINARY_API_SECRET")?,
        })
    }
}

#[derive(Debug)]
struct ValidatedMedia {
    bytes: Vec<u8>,
    file_name: String,
    content_type: &'static str,
}

#[derive(Debug, Deserialize)]
struct CloudinaryUpload {
    secure_url: String,
    public_id: String,
    resource_type: String,
    bytes: u64,
}

pub async fn upload(tenant_id: &str, mut multipart: Multipart) -> ApiResult<Value> {
    let media = read_media(&mut multipart).await?;
    let folder = tenant_folder(tenant_id)?;
    let config = CloudinaryConfig::from_environment().map_err(|error| {
        tracing::error!(error = ?error, "Cloudinary media storage is not configured");
        ApiError::ServiceUnavailable("Media storage is not configured".into())
    })?;
    let uploaded = upload_to_cloudinary(&config, &folder, media)
        .await
        .map_err(|error| {
            tracing::error!(error = ?error, tenant = tenant_id, "Cloudinary upload failed");
            ApiError::BadGateway("Media storage rejected the upload".into())
        })?;

    if !uploaded.secure_url.starts_with("https://")
        || !uploaded.public_id.starts_with(&format!("{folder}/"))
    {
        tracing::error!(
            tenant = tenant_id,
            public_id = uploaded.public_id,
            "Cloudinary returned an invalid tenant media reference"
        );
        return Err(ApiError::BadGateway(
            "Media storage returned an invalid asset reference".into(),
        ));
    }

    Ok(json!({
        "secureUrl": uploaded.secure_url,
        "publicId": uploaded.public_id,
        "resourceType": uploaded.resource_type,
        "bytes": uploaded.bytes,
    }))
}

async fn read_media(multipart: &mut Multipart) -> ApiResult<ValidatedMedia> {
    let mut selected = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("Invalid multipart upload".into()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        if selected.is_some() {
            return Err(ApiError::BadRequest(
                "Upload exactly one file per request".into(),
            ));
        }
        let file_name = field.file_name().unwrap_or("upload").to_owned();
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::BadRequest("Could not read uploaded file".into()))?;
        if bytes.is_empty() {
            return Err(ApiError::BadRequest("Uploaded file is empty".into()));
        }
        if bytes.len() > MAX_MEDIA_BYTES {
            return Err(ApiError::BadRequest(
                "Images and PDFs must not exceed 10 MB".into(),
            ));
        }
        let content_type = detect_media_type(&bytes).ok_or_else(|| {
            ApiError::BadRequest("Only JPEG, PNG, GIF, WebP, and PDF files are supported".into())
        })?;
        selected = Some(ValidatedMedia {
            bytes: bytes.to_vec(),
            file_name,
            content_type,
        });
    }

    selected.ok_or_else(|| ApiError::BadRequest("Multipart field 'file' is required".into()))
}

async fn upload_to_cloudinary(
    config: &CloudinaryConfig,
    folder: &str,
    media: ValidatedMedia,
) -> anyhow::Result<CloudinaryUpload> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let allowed_formats = "jpg,jpeg,png,gif,webp,pdf";
    let signature = cloudinary_signature(
        &[
            ("allowed_formats", allowed_formats),
            ("folder", folder),
            ("timestamp", &timestamp.to_string()),
        ],
        &config.api_secret,
    );
    let file = Part::bytes(media.bytes)
        .file_name(media.file_name)
        .mime_str(media.content_type)?;
    let form = Form::new()
        .part("file", file)
        .text("api_key", config.api_key.clone())
        .text("timestamp", timestamp.to_string())
        .text("folder", folder.to_owned())
        .text("allowed_formats", allowed_formats)
        .text("signature", signature);
    let url = format!(
        "https://api.cloudinary.com/v1_1/{}/auto/upload",
        config.cloud_name
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Cloudinary HTTP client could not be created")?;
    let response = client
        .post(url)
        .multipart(form)
        .send()
        .await
        .context("Cloudinary request failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("Cloudinary returned {status}: {body}");
    }
    response
        .json()
        .await
        .context("Cloudinary response was invalid")
}

fn required_environment(name: &str) -> anyhow::Result<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{name} is required for media uploads"))
}

fn tenant_folder(tenant_id: &str) -> ApiResult<String> {
    let tenant = tenant_id.trim();
    if tenant.is_empty()
        || !tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError::BadRequest("Invalid tenant media scope".into()));
    }
    Ok(format!("supercampus/{tenant}/media"))
}

fn cloudinary_signature(parameters: &[(&str, &str)], api_secret: &str) -> String {
    let mut parameters = parameters.to_vec();
    parameters.sort_unstable_by_key(|(key, _)| *key);
    let payload = parameters
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let digest = Sha1::digest(format!("{payload}{api_secret}").as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_detection_does_not_trust_a_file_extension() {
        assert_eq!(detect_media_type(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(
            detect_media_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(detect_media_type(b"not really a photo.jpg"), None);
    }

    #[test]
    fn tenant_folder_rejects_path_injection() {
        assert_eq!(
            tenant_folder("tenant-local").expect("tenant folder"),
            "supercampus/tenant-local/media"
        );
        assert!(tenant_folder("../another-tenant").is_err());
        assert!(tenant_folder("tenant/local").is_err());
    }

    #[test]
    fn signature_is_sorted_and_stable() {
        assert_eq!(
            cloudinary_signature(
                &[
                    ("timestamp", "1700000000"),
                    ("folder", "supercampus/tenant-local/media"),
                ],
                "secret"
            ),
            "cbe75e617563de8575aa0dbc9ca3be2d7e7cafb1"
        );
    }
}
