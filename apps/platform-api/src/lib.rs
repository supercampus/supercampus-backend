pub mod dashboard;
pub mod error;
pub mod models;
pub mod routes;
pub mod state;
mod media;

use std::net::SocketAddr;

use anyhow::Context;
use axum::middleware;
use routes::router;
use state::AppState;
use supercampus_authn::{AuthConfig, AuthService};
use supercampus_database::{Database, TenantDatabaseManager};
use tower_http::trace::TraceLayer;

use axum::http::{HeaderName, HeaderValue, Method, header};
use tower_http::cors::{AllowOrigin, CorsLayer};

pub fn app(state: AppState) -> axum::Router {
    let tenant_databases = state.tenant_databases();
    let auth_state = state.clone();
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
        .layer(TraceLayer::new_for_http())
}

fn cors_layer() -> CorsLayer {
    let origins_var = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let origins: Vec<HeaderValue> = origins_var
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _request_head| {
                origin
                    .to_str()
                    .is_ok_and(|value| is_allowed_cors_origin(value, &origins))
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
        ])
        .allow_credentials(true)
}

fn is_allowed_cors_origin(origin: &str, configured: &[HeaderValue]) -> bool {
    if configured
        .iter()
        .any(|allowed| allowed.to_str().is_ok_and(|value| value == origin))
    {
        return true;
    }
    origin.starts_with("http://localhost:")
        || origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("http://10.0.2.2:")
}

pub async fn run() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("platform-api");
    let auth = auth_service_from_environment()?;
    let control_database_url =
        std::env::var("CONTROL_DATABASE_URL").context("CONTROL_DATABASE_URL is required")?;
    let control_database = Database::connect(&control_database_url).await?;
    control_database.migrate().await?;
    tracing::info!("control database migration check completed");
    let tenant_databases =
        TenantDatabaseManager::clustered(control_database.clone(), &control_database_url)?;
    tracing::info!("tenant database manager initialized");
    let mailer = supercampus_notifications::mailer_from_environment()?;
    tracing::info!(
        transport = mailer.transport(),
        "outbound email transport ready"
    );
    let state = AppState::with_tenant_databases(tenant_databases.clone())
        .with_auth(auth)
        .with_mailer(mailer);
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
