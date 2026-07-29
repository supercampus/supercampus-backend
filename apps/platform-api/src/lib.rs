pub mod error;
pub mod models;
pub mod routes;
pub mod state;

use std::net::SocketAddr;

use anyhow::Context;
use routes::router;
use state::AppState;
use supercampus_database::Database;
use tower_http::trace::TraceLayer;

pub fn app(state: AppState) -> axum::Router {
    router(state).layer(TraceLayer::new_for_http())
}

pub async fn run() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    supercampus_observability::init("platform-api");
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let database = Database::connect(&database_url).await?;
    database.migrate().await?;
    database.ping().await?;
    tracing::info!(storage = "postgresql", "SuperCampus storage connected");

    let host = std::env::var("HTTP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("HTTP_PORT").unwrap_or_else(|_| "4000".into());
    let address: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid HTTP address: {host}:{port}"))?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "SuperCampus platform API listening");
    axum::serve(listener, app(AppState::with_database(database))).await?;
    Ok(())
}
