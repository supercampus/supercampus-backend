use axum::{
    Router,
    routing::{get, post},
};
use supercampus_database::TenantDatabaseManager;

use super::handlers::{self, DeskApiState};

pub fn router(databases: Option<TenantDatabaseManager>) -> Router {
    let state = DeskApiState { databases };

    Router::new()
        .route("/health", get(handlers::health))
        .route(
            "/cases",
            get(handlers::list_cases).post(handlers::open_case),
        )
        .route("/cases/{id}/actions", post(handlers::act_on_case))
        .with_state(state)
}
