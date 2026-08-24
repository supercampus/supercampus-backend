use axum::{
    Router,
    routing::{get, post, put},
};
use supercampus_database::TenantDatabaseManager;

use crate::application::CrmService;

use super::handlers::{self, CrmApiState};

pub fn router(databases: Option<TenantDatabaseManager>) -> Router {
    let (realtime_wake, _) = tokio::sync::broadcast::channel(256);
    let state = CrmApiState {
        databases,
        catalog_service: CrmService::new(None),
        realtime_wake,
    };

    Router::new()
        .route("/health", get(handlers::health))
        .route("/roles", get(handlers::roles))
        .route(
            "/permissions/effective",
            get(handlers::effective_permissions),
        )
        .route(
            "/leads",
            get(handlers::list_leads).post(handlers::create_lead),
        )
        .route("/leads/import", post(handlers::import_leads))
        .route("/leads/unassigned", get(handlers::list_unassigned_leads))
        .route(
            "/leads/{id}",
            get(handlers::get_lead)
                .patch(handlers::update_lead)
                .delete(handlers::delete_lead),
        )
        .route("/leads/{id}/assign", post(handlers::assign_lead))
        .route("/leads/{id}/transfer", post(handlers::transfer_lead))
        .route(
            "/pipeline/transfer-candidates",
            get(handlers::transfer_candidates),
        )
        .route("/leads/{id}/claim", post(handlers::claim_lead))
        .route("/leads/{id}/reassign", post(handlers::reassign_lead))
        .route("/leads/{id}/stage/move", post(handlers::move_stage))
        .route(
            "/leads/{id}/stage/request",
            post(handlers::request_stage_move),
        )
        .route("/move-requests", get(handlers::list_move_requests))
        .route(
            "/move-requests/{id}/approve",
            post(handlers::approve_move_request),
        )
        .route(
            "/move-requests/{id}/reject",
            post(handlers::reject_move_request),
        )
        .route("/leads/{id}/stage/prospect", post(handlers::mark_prospect))
        .route("/leads/{id}/prospect", post(handlers::mark_prospect))
        .route("/leads/{id}/stage/defer", post(handlers::mark_deferred))
        .route("/leads/{id}/defer", post(handlers::mark_deferred))
        .route("/leads/{id}/stage/hold", post(handlers::hold))
        .route("/leads/{id}/hold", post(handlers::hold))
        .route(
            "/leads/{id}/stage/release-hold",
            post(handlers::release_hold),
        )
        .route("/leads/{id}/release-hold", post(handlers::release_hold))
        .route("/leads/{id}/stage/archive", post(handlers::archive))
        .route("/leads/{id}/archive", post(handlers::archive))
        .route("/leads/{id}/stage/unarchive", post(handlers::unarchive))
        .route("/leads/{id}/unarchive", post(handlers::unarchive))
        .route("/leads/{id}/timeline", get(handlers::timeline))
        .route("/leads/{id}/notes", post(handlers::add_lead_note))
        .route("/leads/{id}/tasks", post(handlers::add_lead_task))
        .route("/leads/{id}/application", get(handlers::application_link))
        .route(
            "/leads/{id}/application-invitations",
            post(handlers::create_application_invitation),
        )
        .route("/kanban/board", get(handlers::board))
        .route("/kanban/my-board", get(handlers::my_board))
        .route("/kanban/stages", get(handlers::stages))
        .route("/kanban/stages/{stage}/leads", get(handlers::stage_leads))
        .route("/kanban/stages/{stage}/count", get(handlers::stage_count))
        .route("/dashboard", get(handlers::board))
        .route("/dashboard/operations", get(handlers::operations_dashboard))
        .route("/activity", get(handlers::recent_activity))
        .route("/assistant/text", post(handlers::text_assistant))
        .route("/events", get(handlers::realtime_events))
        .route(
            "/forms",
            get(handlers::list_forms).post(handlers::create_form),
        )
        .route("/forms/published", get(handlers::published_forms))
        .route(
            "/forms/published/lead-capture",
            get(handlers::published_lead_capture_form),
        )
        // Must stay after the literal lead-capture route so that path keeps priority.
        .route(
            "/forms/published/type/{form_type}",
            get(handlers::published_form_by_type),
        )
        .route(
            "/forms/{id}",
            get(handlers::get_form)
                .put(handlers::update_form)
                .patch(handlers::update_form)
                .delete(handlers::delete_form),
        )
        .route("/forms/{id}/publish", post(handlers::publish_form))
        .route("/forms/{id}/unpublish", post(handlers::unpublish_form))
        .route("/forms/{id}/submit", post(handlers::submit_form))
        .route(
            "/public/forms/{id}/submit",
            post(handlers::submit_public_form),
        )
        .route(
            "/public/applications/{token}",
            get(handlers::public_application_invitation),
        )
        .route(
            "/public/applications/{token}/verify",
            post(handlers::verify_application_otp),
        )
        .route(
            "/public/applications/{token}/submit",
            post(handlers::submit_invited_application),
        )
        .route("/forms/{id}/submissions", get(handlers::form_submissions))
        .route("/communications/whatsapp", post(handlers::send_whatsapp))
        .route("/communications/email", post(handlers::send_email))
        .route("/communications/calls", post(handlers::log_call))
        .route(
            "/templates",
            get(handlers::list_templates).post(handlers::create_template),
        )
        .route(
            "/communications/templates",
            get(handlers::list_templates).post(handlers::create_template),
        )
        .route(
            "/assignment/counselors",
            get(handlers::list_counselors).put(handlers::upsert_counselor),
        )
        .route(
            "/campaigns",
            get(handlers::list_campaigns).post(handlers::upsert_campaign),
        )
        .route("/configuration", get(handlers::configuration))
        .route(
            "/configuration/workflow-toggles",
            put(handlers::workflow_toggle),
        )
        .route(
            "/configuration/automation-toggles",
            put(handlers::automation_toggle),
        )
        .with_state(state)
}
