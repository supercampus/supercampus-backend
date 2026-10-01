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
use supercampus_database::Database;
use uuid::Uuid;

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
        let individual = (
            optional_environment("CLOUDINARY_CLOUD_NAME"),
            optional_environment("CLOUDINARY_API_KEY"),
            optional_environment("CLOUDINARY_API_SECRET"),
        );
        match individual {
            (Some(cloud_name), Some(api_key), Some(api_secret)) => Ok(Self {
                cloud_name,
                api_key,
                api_secret,
            }),
            (None, None, None) => parse_cloudinary_url(&required_environment("CLOUDINARY_URL")?),
            _ => bail!(
                "set all of CLOUDINARY_CLOUD_NAME, CLOUDINARY_API_KEY, and CLOUDINARY_API_SECRET, or set CLOUDINARY_URL"
            ),
        }
    }
}

/// Where uploaded files go.
///
/// `MEDIA_STORAGE=cloudinary` insists on Cloudinary and refuses to start
/// without it; `MEDIA_STORAGE=database` keeps every upload in the tenant's own
/// database. Unset, Cloudinary is used when it is configured and the tenant
/// database takes over when it is not, or when Cloudinary refuses an upload,
/// so an announcement attachment never fails just because the CDN account is
/// missing or misconfigured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageMode {
    Auto,
    Cloudinary,
    Database,
}

fn storage_mode() -> anyhow::Result<StorageMode> {
    match optional_environment("MEDIA_STORAGE")
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("auto") => Ok(StorageMode::Auto),
        Some("cloudinary") => Ok(StorageMode::Cloudinary),
        Some("database") => Ok(StorageMode::Database),
        Some(other) => bail!("MEDIA_STORAGE must be auto, cloudinary or database, not {other:?}"),
    }
}

pub fn validate_configuration() -> anyhow::Result<()> {
    match storage_mode()? {
        StorageMode::Cloudinary => CloudinaryConfig::from_environment().map(|_| ()),
        StorageMode::Database => {
            tracing::info!("media uploads are stored in the tenant database (MEDIA_STORAGE=database)");
            Ok(())
        }
        StorageMode::Auto => {
            if let Err(error) = CloudinaryConfig::from_environment() {
                tracing::warn!(
                    error = %error,
                    "Cloudinary is not configured; media uploads fall back to the tenant database"
                );
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
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

/// Stores one uploaded file and returns its public reference.
///
/// `public_base` is the API's own origin (for example
/// `https://api.supercampus.ai`); files kept in the database are served from
/// `{public_base}/api/media/files/...`.
pub async fn upload(
    tenant_id: &str,
    database: &Database,
    public_base: &str,
    uploaded_by: Option<&str>,
    mut multipart: Multipart,
) -> ApiResult<Value> {
    let media = read_media(&mut multipart).await?;
    let folder = tenant_folder(tenant_id)?;
    let mode = storage_mode().map_err(|error| {
        tracing::error!(error = ?error, "media storage mode is invalid");
        ApiError::ServiceUnavailable("Media storage is not configured".into())
    })?;
    let config = match mode {
        StorageMode::Database => None,
        StorageMode::Cloudinary => Some(CloudinaryConfig::from_environment().map_err(|error| {
            tracing::error!(error = ?error, "Cloudinary media storage is not configured");
            ApiError::ServiceUnavailable("Media storage is not configured".into())
        })?),
        StorageMode::Auto => CloudinaryConfig::from_environment().ok(),
    };
    let Some(config) = config else {
        return store_in_database(tenant_id, database, public_base, uploaded_by, media).await;
    };
    // Cloudinary consumes the bytes; keep a copy only when a fallback may need it.
    let fallback = (mode == StorageMode::Auto).then(|| media.clone());
    let uploaded = match upload_to_cloudinary(&config, &folder, media).await {
        Ok(uploaded) => uploaded,
        Err(error) => {
            tracing::error!(error = ?error, tenant = tenant_id, "Cloudinary upload failed");
            if let Some(media) = fallback {
                tracing::warn!(
                    tenant = tenant_id,
                    "storing the upload in the tenant database instead of Cloudinary"
                );
                return store_in_database(tenant_id, database, public_base, uploaded_by, media)
                    .await;
            }
            return Err(ApiError::BadGateway(
                "Media storage rejected the upload. Try again, or contact your administrator."
                    .into(),
            ));
        }
    };

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

/// Stores bytes the server produced itself.
///
/// The multipart path above exists for files a person chose; this one is for
/// images the platform renders — a visitor's gate pass, which has to live at a
/// public URL because Twilio fetches it to attach to the message. It shares the
/// tenant folder and the signing, so a rendered pass is scoped exactly as an
/// uploaded file is.
pub async fn store_rendered_png(
    tenant_id: &str,
    file_name: &str,
    bytes: Vec<u8>,
) -> ApiResult<Value> {
    if bytes.len() > MAX_MEDIA_BYTES {
        return Err(ApiError::BadRequest("That image is too large".into()));
    }
    let folder = tenant_folder(tenant_id)?;
    let config = CloudinaryConfig::from_environment().map_err(|error| {
        tracing::error!(error = ?error, "Cloudinary media storage is not configured");
        ApiError::ServiceUnavailable("Media storage is not configured".into())
    })?;
    let uploaded = upload_to_cloudinary(
        &config,
        &folder,
        ValidatedMedia {
            bytes,
            file_name: file_name.to_owned(),
            content_type: "image/png",
        },
    )
    .await
    .map_err(|error| {
        tracing::error!(error = ?error, tenant = tenant_id, "Cloudinary upload failed");
        ApiError::BadGateway(format!("Media storage rejected the upload: {error:#}"))
    })?;

    Ok(json!({
        "secureUrl": uploaded.secure_url,
        "publicId": uploaded.public_id,
        "bytes": uploaded.bytes,
    }))
}

async fn store_in_database(
    tenant_id: &str,
    database: &Database,
    public_base: &str,
    uploaded_by: Option<&str>,
    media: ValidatedMedia,
) -> ApiResult<Value> {
    let tenant = tenant_folder(tenant_id).map(|_| tenant_id.trim().to_owned())?;
    let id = Uuid::new_v4();
    let file_name = public_file_name(&media.file_name, media.content_type);
    let size = media.bytes.len();
    let byte_size =
        i32::try_from(size).map_err(|_| ApiError::BadRequest("That file is too large".into()))?;
    let insert = || {
        sqlx::query(
            r#"INSERT INTO campus_ops.media_objects
               (id, tenant_slug, file_name, content_type, byte_size, content, uploaded_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(id)
        .bind(&tenant)
        .bind(&file_name)
        .bind(media.content_type)
        .bind(byte_size)
        .bind(&media.bytes)
        .bind(uploaded_by)
        .execute(database.pool())
    };
    let mut result = insert().await;
    if result.as_ref().is_err_and(is_undefined_table) {
        // The sqlx migrator stops at the first duplicate version prefix in
        // migrations/runtime and swallows the error, so a database can miss
        // 0116. The DDL is idempotent; apply it and try once more.
        tracing::warn!(tenant = tenant_id, "creating campus_ops.media_objects on first use");
        if let Err(error) = sqlx::raw_sql(MEDIA_OBJECTS_DDL).execute(database.pool()).await {
            tracing::error!(error = ?error, tenant = tenant_id, "could not create campus_ops.media_objects");
        }
        result = insert().await;
    }
    result.map_err(|error| {
        tracing::error!(error = ?error, tenant = tenant_id, "database media storage failed");
        ApiError::ServiceUnavailable(
            "The file could not be stored right now. Try again in a moment.".into(),
        )
    })?;
    let base = public_base.trim_end_matches('/');
    Ok(json!({
        "secureUrl": format!("{base}/api/media/files/{tenant}/{id}/{file_name}"),
        "publicId": format!("db:{id}"),
        "resourceType": if media.content_type == "application/pdf" { "raw" } else { "image" },
        "bytes": size,
        "storage": "database",
    }))
}

const MEDIA_OBJECTS_DDL: &str = include_str!("../../../migrations/runtime/0116_media_objects.sql");

fn is_undefined_table(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "42P01" || code == "3F000")
}

/// One file kept by [`store_in_database`], as served to anyone holding its URL.
///
/// The random id is the capability, exactly as a Cloudinary URL is: walls
/// render these with plain `<img>`/`Image.network` and open PDFs in a browser
/// tab, neither of which can present a bearer token.
pub struct StoredMedia {
    pub file_name: String,
    pub content_type: String,
    pub content: Vec<u8>,
}

pub async fn load_from_database(
    database: &Database,
    tenant_id: &str,
    id: Uuid,
) -> ApiResult<Option<StoredMedia>> {
    let row = sqlx::query_as::<_, (String, String, Vec<u8>)>(
        r#"SELECT file_name, content_type, content FROM campus_ops.media_objects
           WHERE id = $1 AND tenant_slug = $2"#,
    )
    .bind(id)
    .bind(tenant_id.trim())
    .fetch_optional(database.pool())
    .await;
    let row = match row {
        Err(error) if is_undefined_table(&error) => None,
        other => other?,
    };
    Ok(row.map(|(file_name, content_type, content)| StoredMedia {
        file_name,
        content_type,
        content,
    }))
}

pub fn valid_tenant_slug(tenant_id: &str) -> bool {
    tenant_folder(tenant_id).is_ok()
}

/// The origin files are served from: `API_PUBLIC_URL` when set, otherwise the
/// scheme and host the request arrived with (honouring a reverse proxy's
/// forwarded headers).
pub fn public_base_url(headers: &axum::http::HeaderMap) -> String {
    if let Some(configured) = optional_environment("API_PUBLIC_URL") {
        return configured.trim_end_matches('/').to_owned();
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let valid_host = |host: &String| {
        host.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']'))
    };
    let host = header("x-forwarded-host")
        .filter(valid_host)
        .or_else(|| header("host").filter(valid_host))
        .unwrap_or_else(|| "api.supercampus.ai".to_owned());
    let scheme = match header("x-forwarded-proto").as_deref() {
        Some("http") => "http",
        Some(_) => "https",
        None if is_local_host(&host) => "http",
        None => "https",
    };
    format!("{scheme}://{host}")
}

fn is_local_host(host: &str) -> bool {
    let name = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(name, _)| name);
    name == "localhost"
        || name == "[::1]"
        || name.ends_with(".localhost")
        || name.starts_with("127.")
        || name.starts_with("10.")
        || name.starts_with("192.168.")
}

/// A URL-safe file name that keeps the extension the wall uses to tell a PDF
/// from an image.
fn public_file_name(original: &str, content_type: &str) -> String {
    let extension = match content_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        _ => "bin",
    };
    let last = original.rsplit(['/', '\\']).next().unwrap_or(original);
    let base = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    let mut stem: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '-' })
        .collect();
    stem = stem.trim_matches('-').chars().take(80).collect();
    if stem.is_empty() {
        stem = "file".into();
    }
    format!("{stem}.{extension}")
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
            ApiError::BadRequest("Only JPEG, PNG, GIF, WebP, HEIC, and PDF files are supported".into())
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
    let allowed_formats = "jpg,jpeg,png,gif,webp,pdf,heic,heif,avif";
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
        // Cloudinary must follow the host OS trust store. This keeps TLS
        // verification enabled while supporting managed Windows certificates.
        .use_native_tls()
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
    optional_environment(name).ok_or_else(|| anyhow!("{name} is required for media uploads"))
}

fn optional_environment(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_cloudinary_url(value: &str) -> anyhow::Result<CloudinaryConfig> {
    let authority = value
        .strip_prefix("cloudinary://")
        .ok_or_else(|| anyhow!("CLOUDINARY_URL must start with cloudinary://"))?;
    let (credentials, cloud_name) = authority
        .rsplit_once('@')
        .ok_or_else(|| anyhow!("CLOUDINARY_URL must contain credentials and a cloud name"))?;
    let (api_key, api_secret) = credentials
        .split_once(':')
        .ok_or_else(|| anyhow!("CLOUDINARY_URL must contain an API key and secret"))?;
    let cloud_name = cloud_name.split(['/', '?', '#']).next().unwrap_or_default();
    if api_key.is_empty() || api_secret.is_empty() || cloud_name.is_empty() {
        bail!("CLOUDINARY_URL contains an empty API key, secret, or cloud name");
    }
    Ok(CloudinaryConfig {
        cloud_name: percent_decode(cloud_name)?,
        api_key: percent_decode(api_key)?,
        api_secret: percent_decode(api_secret)?,
    })
}

fn percent_decode(value: &str) -> anyhow::Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("CLOUDINARY_URL contains invalid percent encoding");
            }
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3])?;
            decoded.push(u8::from_str_radix(pair, 16).context("invalid percent encoding")?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).context("CLOUDINARY_URL contains invalid UTF-8")
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

/// Largest original the attachment proxy will relay.
pub const MAX_PROXIED_BYTES: usize = 25 * 1024 * 1024;

/// One of this account's SuperCampus assets, read from its delivery URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudinaryAsset {
    resource_type: String,
    public_id: String,
    format: Option<String>,
}

impl CloudinaryAsset {
    /// The file name the asset was delivered under.
    pub fn file_name(&self) -> String {
        let last = self.public_id.rsplit('/').next().unwrap_or("attachment");
        match &self.format {
            Some(format) => format!("{last}.{format}"),
            None => last.to_owned(),
        }
    }
}

/// Reads a `res.cloudinary.com` delivery URL of *this* cloud's SuperCampus
/// media. Anything else — another cloud, another host, a path outside the
/// `supercampus/` folder, a transformation — is refused, so the proxy cannot
/// be used to fetch arbitrary URLs.
fn parse_cloudinary_asset(url: &str, cloud_name: &str) -> Option<CloudinaryAsset> {
    let rest = url
        .strip_prefix("https://res.cloudinary.com/")
        .or_else(|| url.strip_prefix("http://res.cloudinary.com/"))?;
    let rest = rest.split(['?', '#']).next()?;
    let mut segments = rest.split('/');
    if segments.next()? != cloud_name {
        return None;
    }
    let resource_type = segments.next()?;
    if !matches!(resource_type, "image" | "raw" | "video") {
        return None;
    }
    if segments.next()? != "upload" {
        return None;
    }
    let mut remaining: Vec<&str> = segments.collect();
    // An optional version segment (v1727000000) comes before the public id.
    if remaining
        .first()
        .is_some_and(|first| first.len() > 1 && first.starts_with('v') && first[1..].bytes().all(|b| b.is_ascii_digit()))
    {
        remaining.remove(0);
    }
    if remaining.is_empty() || remaining.iter().any(|segment| segment.is_empty() || *segment == "..") {
        return None;
    }
    let decoded: Vec<String> = remaining
        .iter()
        .map(|segment| percent_decode(segment).ok())
        .collect::<Option<_>>()?;
    let path = decoded.join("/");
    if !path.starts_with("supercampus/") {
        return None;
    }
    if resource_type == "raw" {
        return Some(CloudinaryAsset {
            resource_type: resource_type.to_owned(),
            public_id: path,
            format: None,
        });
    }
    let (public_id, format) = path.rsplit_once('.')?;
    if public_id.is_empty() || !format.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(CloudinaryAsset {
        resource_type: resource_type.to_owned(),
        public_id: public_id.to_owned(),
        format: Some(format.to_ascii_lowercase()),
    })
}

/// Fetches an original from Cloudinary through its signed download API.
///
/// New Cloudinary accounts refuse to *deliver* PDFs ("Allow delivery of PDF
/// and ZIP files" is off), so an attachment uploaded there returns 401 to the
/// app. The authenticated download API is not subject to that setting, so
/// the API relays the file instead. Returns None when the URL is not one of
/// this account's SuperCampus assets or Cloudinary is not configured.
pub async fn fetch_cloudinary_original(
    url: &str,
) -> anyhow::Result<Option<(Vec<u8>, &'static str, String)>> {
    let Ok(config) = CloudinaryConfig::from_environment() else {
        return Ok(None);
    };
    let Some(asset) = parse_cloudinary_asset(url, &config.cloud_name) else {
        return Ok(None);
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs()
        .to_string();
    let mut signed: Vec<(&str, &str)> = vec![
        ("public_id", asset.public_id.as_str()),
        ("timestamp", timestamp.as_str()),
        ("type", "upload"),
    ];
    if let Some(format) = &asset.format {
        signed.push(("format", format.as_str()));
    }
    let signature = cloudinary_signature(&signed, &config.api_secret);
    let mut query: Vec<(&str, &str)> = signed.clone();
    query.push(("api_key", config.api_key.as_str()));
    query.push(("signature", signature.as_str()));
    let client = reqwest::Client::builder()
        .use_native_tls()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Cloudinary HTTP client could not be created")?;
    let response = client
        .get(format!(
            "https://api.cloudinary.com/v1_1/{}/{}/download",
            config.cloud_name, asset.resource_type
        ))
        .query(&query)
        .send()
        .await
        .context("Cloudinary download request failed")?;
    if !response.status().is_success() {
        bail!("Cloudinary download returned {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length as usize > MAX_PROXIED_BYTES)
    {
        bail!("attachment is larger than the proxy relays");
    }
    let bytes = response.bytes().await.context("Cloudinary download was cut short")?;
    if bytes.len() > MAX_PROXIED_BYTES {
        bail!("attachment is larger than the proxy relays");
    }
    let content_type = detect_media_type(&bytes).unwrap_or("application/octet-stream");
    Ok(Some((bytes.to_vec(), content_type, asset.file_name())))
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
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(
            &bytes[8..12],
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx"
        )
    {
        Some("image/heic")
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"mif1" | b"msf1")
    {
        Some("image/heif")
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"avif" | b"avis")
    {
        Some("image/avif")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_reads_this_clouds_supercampus_assets_only() {
        let pdf = parse_cloudinary_asset(
            "https://res.cloudinary.com/campus/image/upload/v1727581442/supercampus/mec/media/SCAN_2026.pdf",
            "campus",
        )
        .expect("a SuperCampus PDF");
        assert_eq!(pdf.resource_type, "image");
        assert_eq!(pdf.public_id, "supercampus/mec/media/SCAN_2026");
        assert_eq!(pdf.format.as_deref(), Some("pdf"));
        assert_eq!(pdf.file_name(), "SCAN_2026.pdf");

        let raw = parse_cloudinary_asset(
            "https://res.cloudinary.com/campus/raw/upload/supercampus/mec/media/sheet.csv",
            "campus",
        )
        .expect("a raw asset");
        assert_eq!(raw.public_id, "supercampus/mec/media/sheet.csv");
        assert_eq!(raw.format, None);

        let spaced = parse_cloudinary_asset(
            "https://res.cloudinary.com/campus/image/upload/v1/supercampus/mec/media/Fee%20notice.pdf",
            "campus",
        )
        .expect("a percent-encoded name");
        assert_eq!(spaced.public_id, "supercampus/mec/media/Fee notice");

        for refused in [
            // Another cloud, another host, outside the folder, a transformation.
            "https://res.cloudinary.com/other/image/upload/v1/supercampus/mec/a.pdf",
            "https://evil.example/campus/image/upload/v1/supercampus/mec/a.pdf",
            "https://res.cloudinary.com/campus/image/upload/v1/private/a.pdf",
            "https://res.cloudinary.com/campus/image/upload/w_100/supercampus/a.pdf",
            "https://res.cloudinary.com/campus/image/private/v1/supercampus/a.pdf",
            "https://res.cloudinary.com/campus/image/upload/v1/supercampus/../x.pdf",
            "not a url",
        ] {
            assert_eq!(parse_cloudinary_asset(refused, "campus"), None, "{refused}");
        }
    }

    #[test]
    fn content_detection_does_not_trust_a_file_extension() {
        assert_eq!(detect_media_type(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(
            detect_media_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(
            detect_media_type(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00"),
            Some("image/heic")
        );
        assert_eq!(
            detect_media_type(b"\x00\x00\x00\x1cftypavif\x00\x00\x00\x00"),
            Some("image/avif")
        );
        assert_eq!(detect_media_type(b"not really a photo.jpg"), None);
    }

    #[test]
    fn database_file_names_are_url_safe_and_keep_the_real_extension() {
        assert_eq!(
            public_file_name("Exam Circular (final).PDF", "application/pdf"),
            "Exam-Circular--final.pdf"
        );
        assert_eq!(public_file_name("../../evil.php", "image/png"), "evil.png");
        assert_eq!(public_file_name("", "image/jpeg"), "file.jpg");
        assert_eq!(public_file_name("photo.jpeg", "image/webp"), "photo.webp");
    }

    #[test]
    fn public_base_url_follows_the_proxy_headers() {
        use axum::http::{HeaderMap, HeaderValue};
        if optional_environment("API_PUBLIC_URL").is_some() {
            return;
        }
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:4000"));
        assert_eq!(public_base_url(&headers), "http://127.0.0.1:4000");
        headers.insert("x-forwarded-host", HeaderValue::from_static("api.supercampus.ai"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert_eq!(public_base_url(&headers), "https://api.supercampus.ai");
        headers.insert("x-forwarded-host", HeaderValue::from_static("evil\"host/x"));
        assert_eq!(public_base_url(&headers), "https://127.0.0.1:4000");
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

    #[test]
    fn parses_the_standard_cloudinary_url() {
        let config = parse_cloudinary_url("cloudinary://123456:secret%2Fvalue@campus-cloud")
            .expect("cloudinary url");
        assert_eq!(config.cloud_name, "campus-cloud");
        assert_eq!(config.api_key, "123456");
        assert_eq!(config.api_secret, "secret/value");
    }
}
