#![forbid(unsafe_code)]

use tracing_subscriber::EnvFilter;

pub fn init(service_name: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .json()
        .try_init();
    tracing::info!(service.name = service_name, "observability initialized");
}
