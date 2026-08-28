pub mod academic_assignments;
pub mod dashboard;
pub mod error;
pub mod governance;
pub mod guardian_link;
pub mod media;
pub mod models;
pub mod operations;
pub mod passes;
pub mod visitors;

pub(crate) use routes::public_base_url;
pub mod realtime;
pub mod routes;
pub mod state;
pub mod timetable;

use std::{net::SocketAddr, time::Duration};

use anyhow::Context;
use axum::middleware;
use routes::router;
use state::AppState;
use supercampus_authn::{AuthConfig, AuthService};
use supercampus_database::{Database, TenantDatabaseManager};
use tower_http::trace::TraceLayer;

use axum::response::Response;
use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, Method, StatusCode, Uri, header},
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;

const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_JSON_BODY_LIMIT: usize = 2 * 1024 * 1024;

pub fn app(state: AppState) -> axum::Router {
    let tenant_databases = state.tenant_databases();
    let auth_state = state.clone();
    let request_id_header = HeaderName::from_static("x-request-id");
    let request_timeout = std::env::var("HTTP_REQUEST_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=120).contains(value))
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECONDS);
    router(state)
        .nest(
            "/api/v1/crm",
            supercampus_crm::api::routes::router(tenant_databases.clone()),
        )
        .nest(
            "/api/v1/application-desk",
            supercampus_application_desk::api::router(tenant_databases),
        )
        .layer(middleware::from_fn_with_state(
            auth_state,
            routes::authorize_request,
        ))
        .layer(cors_layer())
        .layer(DefaultBodyLimit::max(DEFAULT_JSON_BODY_LIMIT))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            Duration::from_secs(request_timeout),
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(middleware::from_fn(security_response_headers))
}

async fn security_response_headers(
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if matches!(
        std::env::var("APP_ENV").as_deref(),
        Ok("production") | Ok("staging")
    ) {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

fn cors_layer() -> CorsLayer {
    let origins_var = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let origins: Vec<HeaderValue> = origins_var
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();
    let allow_local = matches!(
        std::env::var("APP_ENV").as_deref(),
        Ok("development") | Ok("test") | Err(_)
    );

    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _request_head| {
                origin
                    .to_str()
                    .is_ok_and(|value| is_allowed_cors_origin(value, &origins, allow_local))
            },
        ))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::CACHE_CONTROL,
            HeaderName::from_static("pragma"),
            HeaderName::from_static("x-tenant-id"),
            HeaderName::from_static("x-client-surface"),
            HeaderName::from_static("x-application-verification"),
        ])
        .allow_credentials(true)
}

fn is_allowed_cors_origin(origin: &str, configured: &[HeaderValue], allow_local: bool) -> bool {
    // The Flutter web build is deployed as the mobile application test surface.
    // Keep this exact origin narrow: credentials are enabled, so a wildcard is
    // intentionally not used here.
    if origin == "https://supercampusapplication-e0miwj-dcd788-200-141-5-86.sslip.io" {
        return true;
    }
    if configured
        .iter()
        .any(|allowed| allowed.to_str().is_ok_and(|value| value == origin))
    {
        return true;
    }
    allow_local
        && (origin.starts_with("http://localhost:")
            || origin.starts_with("http://127.0.0.1:")
            || origin.starts_with("http://10.0.2.2:"))
}

pub async fn run() -> anyhow::Result<()> {
    if dotenvy::dotenv().is_err() {
        eprintln!("warning: could not load .env; check dotenv syntax");
    }
    supercampus_observability::init("platform-api");
    validate_runtime_security_configuration()?;
    media::validate_configuration().context("Cloudinary media storage configuration is invalid")?;
    tracing::info!("Cloudinary media storage configured");
    let auth = auth_service_from_environment()?;
    let control_database_url =
        std::env::var("CONTROL_DATABASE_URL").context("CONTROL_DATABASE_URL is required")?;
    let control_max_connections = parse_u32_environment("DATABASE_MAX_CONNECTIONS", 10, 1, 100)?;
    let tenant_max_connections =
        parse_u32_environment("TENANT_DATABASE_MAX_CONNECTIONS", 5, 1, 100)?;
    let control_database =
        Database::connect_with_max_connections(&control_database_url, control_max_connections)
            .await?;
    if std::env::var("SKIP_STARTUP_MIGRATIONS")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"))
    {
        tracing::warn!(
            "control database migration check skipped; migrations must be managed by the release job"
        );
        // Dokploy currently starts the API without a separate release job. Keep
        // these accountant rollouts available as idempotent compatibility
        // patches so the deployed credentials, permissions, and UI cannot diverge.
        sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0071_accountant_wallet_access.sql"
        ))
        .execute(control_database.pool())
        .await
        .context("failed to apply the accountant wallet access release patch")?;
        sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0073_abhinaya_accountant_portal.sql"
        ))
        .execute(control_database.pool())
        .await
        .context("failed to apply the Abhinaya accountant release patch")?;
        sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0074_gate_security_portal.sql"
        ))
        .execute(control_database.pool())
        .await
        .context("failed to apply the gate security portal release patch")?;
        sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0075_mec_canteen_captains.sql"
        ))
        .execute(control_database.pool())
        .await
        .context("failed to apply the canteen captain release patch")?;
    } else {
        control_database.migrate().await?;
        tracing::info!("control database migration check completed");
    }
    let tenant_databases = TenantDatabaseManager::clustered_with_max_connections(
        control_database.clone(),
        &control_database_url,
        tenant_max_connections,
    )?;
    if std::env::var("SKIP_STARTUP_MIGRATIONS")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"))
    {
        let mec_database = tenant_databases
            .tenant("mec")
            .await
            .context("failed to open the MEC tenant database for release patches")?;
        sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0076_mec_canteen_captain_shop_assignments.sql"
        ))
        .execute(mec_database.pool())
        .await
        .context("failed to apply MEC canteen captain shop assignments")?;
    }
    tracing::info!("tenant database manager initialized");
    let mailer = supercampus_notifications::mailer_from_environment()?;
    tracing::info!(
        transport = mailer.transport(),
        "outbound email transport ready"
    );
    let whatsapp = supercampus_notifications::whatsapp::whatsapp_from_environment()?;
    tracing::info!(
        transport = whatsapp.transport(),
        "outbound WhatsApp transport ready"
    );
    let state = AppState::with_tenant_databases(tenant_databases.clone())
        .with_auth(auth)
        .with_mailer(mailer)
        .with_whatsapp(whatsapp);
    let seeded = state.seed_test_identities_from_environment().await?;
    if seeded > 0 {
        tracing::info!(count = seeded, "testing identities seeded from environment");
    }
    if std::env::var("SKIP_TENANT_DB_PING").as_deref() == Ok("true") {
        tracing::warn!("registered tenant database startup ping skipped");
    } else {
        tenant_databases.ping_registered().await?;
    }
    tracing::info!(storage = "postgresql", "SuperCampus storage connected");
    let host = std::env::var("HTTP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("HTTP_PORT").unwrap_or_else(|_| "4000".into());
    let address: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid HTTP address: {host}:{port}"))?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "SuperCampus platform API listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn validate_runtime_security_configuration() -> anyhow::Result<()> {
    let environment = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
    if environment != "production" && environment != "staging" {
        return Ok(());
    }

    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is required")?;
    anyhow::ensure!(
        jwt_secret.len() >= 32
            && !jwt_secret.contains("change-in-production")
            && !jwt_secret.contains("replace-with"),
        "JWT_SECRET must be a unique production secret with at least 32 characters"
    );
    anyhow::ensure!(
        std::env::var("SEED_TEST_USERS").as_deref() != Ok("true"),
        "SEED_TEST_USERS must be false in production"
    );
    anyhow::ensure!(
        std::env::var("SKIP_TENANT_DB_PING").as_deref() != Ok("true"),
        "SKIP_TENANT_DB_PING must be false in production"
    );

    let public_url = std::env::var("APP_PUBLIC_URL").context("APP_PUBLIC_URL is required")?;
    validate_https_origin("APP_PUBLIC_URL", &public_url)?;

    let cors_origins =
        std::env::var("CORS_ALLOWED_ORIGINS").context("CORS_ALLOWED_ORIGINS is required")?;
    let configured: Vec<&str> = cors_origins
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    anyhow::ensure!(
        !configured.is_empty(),
        "CORS_ALLOWED_ORIGINS must contain at least one HTTPS origin"
    );
    for origin in configured {
        validate_https_origin("CORS_ALLOWED_ORIGINS", origin)?;
    }

    let jwt_issuer = std::env::var("JWT_ISSUER").context("JWT_ISSUER is required")?;
    validate_https_origin("JWT_ISSUER", &jwt_issuer)?;
    let jwt_audience = std::env::var("JWT_AUDIENCE").context("JWT_AUDIENCE is required")?;
    anyhow::ensure!(
        !jwt_audience.trim().is_empty(),
        "JWT_AUDIENCE must not be empty"
    );

    let access_ttl = parse_i64_environment("JWT_ACCESS_TTL_SECONDS", 15 * 60)?;
    let refresh_ttl = parse_i64_environment("SESSION_REFRESH_TTL_SECONDS", 30 * 24 * 60 * 60)?;
    anyhow::ensure!(
        (60..=60 * 60).contains(&access_ttl),
        "JWT_ACCESS_TTL_SECONDS must be between 60 and 3600 seconds"
    );
    anyhow::ensure!(
        refresh_ttl > access_ttl && refresh_ttl <= 90 * 24 * 60 * 60,
        "SESSION_REFRESH_TTL_SECONDS must exceed the access-token lifetime and be at most 90 days"
    );

    let request_timeout = std::env::var("HTTP_REQUEST_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| DEFAULT_REQUEST_TIMEOUT_SECONDS.to_string())
        .parse::<u64>()
        .context("HTTP_REQUEST_TIMEOUT_SECONDS must be an integer")?;
    anyhow::ensure!(
        (1..=120).contains(&request_timeout),
        "HTTP_REQUEST_TIMEOUT_SECONDS must be between 1 and 120 seconds"
    );
    Ok(())
}

fn validate_https_origin(name: &str, value: &str) -> anyhow::Result<()> {
    let uri: Uri = value
        .parse()
        .with_context(|| format!("{name} contains an invalid URL"))?;
    anyhow::ensure!(uri.scheme_str() == Some("https"), "{name} must use HTTPS");
    anyhow::ensure!(uri.authority().is_some(), "{name} must include a hostname");
    anyhow::ensure!(
        uri.path_and_query()
            .is_none_or(|part| part.path() == "/" && part.query().is_none()),
        "{name} entries must be origins without a path or query"
    );
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, commencing graceful shutdown");
}

fn auth_service_from_environment() -> anyhow::Result<AuthService> {
    let app_environment = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
    let secret = match std::env::var("JWT_SECRET") {
        Ok(secret) => secret,
        Err(_) if app_environment == "development" || app_environment == "test" => {
            tracing::warn!("JWT_SECRET is not set; using the local-development signing key");
            AuthConfig::development().secret
        }
        Err(error) => return Err(error).context("JWT_SECRET is required outside development"),
    };
    let access_token_ttl_seconds = parse_i64_environment("JWT_ACCESS_TTL_SECONDS", 15 * 60)?;
    let refresh_token_ttl_seconds =
        parse_i64_environment("SESSION_REFRESH_TTL_SECONDS", 30 * 24 * 60 * 60)?;
    AuthService::new(AuthConfig {
        issuer: std::env::var("JWT_ISSUER")
            .unwrap_or_else(|_| "https://auth.supercampus.local".into()),
        audience: std::env::var("JWT_AUDIENCE").unwrap_or_else(|_| "supercampus-api".into()),
        secret,
        access_token_ttl_seconds,
        refresh_token_ttl_seconds,
    })
    .context("invalid JWT/session configuration")
}

fn parse_i64_environment(name: &str, default: i64) -> anyhow::Result<i64> {
    std::env::var(name).map_or(Ok(default), |value| {
        value
            .parse::<i64>()
            .with_context(|| format!("{name} must be an integer number of seconds"))
    })
}

fn parse_u32_environment(
    name: &str,
    default: u32,
    minimum: u32,
    maximum: u32,
) -> anyhow::Result<u32> {
    let value = std::env::var(name).ok().map_or(Ok(default), |raw| {
        raw.parse::<u32>()
            .with_context(|| format!("{name} must be an integer"))
    })?;
    anyhow::ensure!(
        (minimum..=maximum).contains(&value),
        "{name} must be between {minimum} and {maximum}"
    );
    Ok(value)
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn production_cors_does_not_implicitly_allow_localhost() {
        assert!(!is_allowed_cors_origin("http://localhost:3000", &[], false));
        assert!(is_allowed_cors_origin("http://localhost:3000", &[], true));
    }

    #[test]
    fn mobile_application_origin_is_allowed_in_production() {
        assert!(is_allowed_cors_origin(
            "https://supercampusapplication-e0miwj-dcd788-200-141-5-86.sslip.io",
            &[],
            false,
        ));
    }

    #[test]
    fn configured_production_origin_is_exact() {
        let origins = vec![HeaderValue::from_static("https://supercampus.ai")];
        assert!(is_allowed_cors_origin(
            "https://supercampus.ai",
            &origins,
            false
        ));
        assert!(!is_allowed_cors_origin(
            "https://supercampus.ai.attacker.example",
            &origins,
            false
        ));
    }

    #[test]
    fn production_origins_require_https_and_no_path() {
        assert!(validate_https_origin("TEST", "https://supercampus.ai").is_ok());
        assert!(validate_https_origin("TEST", "http://supercampus.ai").is_err());
        assert!(validate_https_origin("TEST", "https://supercampus.ai/api").is_err());
    }

    #[test]
    fn request_timeout_defaults_to_a_bounded_value() {
        assert!((1..=120).contains(&DEFAULT_REQUEST_TIMEOUT_SECONDS));
    }
}
