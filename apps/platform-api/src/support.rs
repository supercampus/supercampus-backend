//! Help requests raised from the app and routed to the people who handle them.
//!
//! A request is assigned to a role, never to a person: every holder of that role
//! sees it in their inbox and gets the alert, and any of them can move it along.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{notify_tx, tenant_id},
    state::{AppState, AuthPrincipal},
};

/// Where `app` requests are also emailed.
const SUPPORT_EMAIL: &str = "support@supercampus.ai";
/// Every tenant has this role, so a request is never left without an owner.
const FALLBACK_ROLE: &str = "tenant_admin";
const SUBJECT_CHARS: std::ops::RangeInclusive<usize> = 3..=120;
const MESSAGE_CHARS: std::ops::RangeInclusive<usize> = 10..=2000;
const NOTE_MAX_CHARS: usize = 2000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/support/categories", get(categories))
        .route("/support/tickets", get(own_tickets).post(create_ticket))
        .route("/support/tickets/{ticket_id}", put(update_ticket))
        .route("/support/inbox", get(inbox))
}

struct Category {
    key: &'static str,
    label: &'static str,
    /// Role keys in order of preference; the first one somebody in the tenant
    /// actually holds wins, otherwise [`FALLBACK_ROLE`].
    roles: &'static [&'static str],
}

const CATEGORIES: [Category; 8] = [
    Category {
        key: "fees",
        label: "Fees & payments",
        roles: &["accountant"],
    },
    Category {
        key: "hostel",
        label: "Hostel & mess",
        roles: &["warden"],
    },
    Category {
        key: "gatepass",
        label: "Gatepass & campus exit",
        roles: &["warden"],
    },
    Category {
        key: "library",
        label: "Library",
        roles: &["librarian"],
    },
    Category {
        key: "academics",
        label: "Attendance, marks & timetable",
        roles: &["hod"],
    },
    Category {
        key: "canteen",
        label: "Canteen, stationery & laundry",
        // `owner` is the role every campus shop owner (canteen, stationery,
        // laundry) holds; the accounts office handles it where no shop owner exists.
        roles: &["owner", "accountant"],
    },
    Category {
        key: "app",
        label: "App or account problem",
        roles: &[FALLBACK_ROLE],
    },
    Category {
        key: "other",
        label: "Something else",
        roles: &[FALLBACK_ROLE],
    },
];

fn category(key: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|category| category.key == key)
}

fn category_label(key: &str) -> &'static str {
    category(key).map_or("Something else", |category| category.label)
}

/// The role a category is routed to, given the role keys the tenant actually has holders for.
fn assigned_role(category: &Category, available_roles: &[String]) -> &'static str {
    category
        .roles
        .iter()
        .copied()
        .find(|role| available_roles.iter().any(|available| available == role))
        .unwrap_or(FALLBACK_ROLE)
}

/// What the requester is told: "Goes to: <handled by>".
fn handled_by(role: &str) -> &'static str {
    match role {
        "accountant" => "Accounts office",
        "warden" => "Hostel warden",
        "librarian" => "Librarian",
        "hod" => "Head of department",
        "owner" => "Shop owner",
        _ => "College admin office",
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "open" => "open",
        "in_progress" => "in progress",
        "resolved" => "resolved",
        _ => "closed",
    }
}

fn is_admin(principal: &AuthPrincipal) -> bool {
    principal
        .roles
        .iter()
        .any(|role| matches!(role.as_str(), "tenant_admin" | "superadmin"))
}

/// Candidate role keys that at least one active member of the tenant holds.
async fn available_roles(state: &AppState, tenant_slug: &str) -> ApiResult<Vec<String>> {
    let mut candidates: Vec<&str> = CATEGORIES
        .iter()
        .flat_map(|category| category.roles.iter().copied())
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    let Some(control) = state.database() else {
        return Ok(Vec::new());
    };
    Ok(sqlx::query_scalar::<_, String>(
        r#"SELECT candidate.role_key
           FROM unnest($2::text[]) AS candidate(role_key)
           JOIN platform.tenants tenant ON tenant.slug = $1
           WHERE EXISTS (
                   SELECT 1 FROM authz.roles role
                   WHERE role.tenant_id = tenant.id AND role.role_key = candidate.role_key
                     AND role.active)
             AND EXISTS (
                   SELECT 1 FROM identity.tenant_memberships membership
                   JOIN identity.users account ON account.id = membership.user_id
                   WHERE membership.tenant_id = tenant.id AND membership.active
                     AND account.active AND candidate.role_key = ANY(membership.roles))"#,
    )
    .bind(tenant_slug)
    .bind(&candidates)
    .fetch_all(control.pool())
    .await?)
}

const TICKET_COLUMNS: &str = "id,category,subject,message,status,assigned_role,resolution_note,\
     requester_name,requester_email,requester_user_id,created_at,updated_at";

fn ticket_json(row: &PgRow) -> ApiResult<Value> {
    let category: String = row.try_get("category")?;
    let assigned_role: String = row.try_get("assigned_role")?;
    let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
    let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?,
        "category": category,
        "categoryLabel": category_label(&category),
        "subject": row.try_get::<String, _>("subject")?,
        "message": row.try_get::<String, _>("message")?,
        "status": row.try_get::<String, _>("status")?,
        "assignedRole": assigned_role,
        "handledBy": handled_by(&assigned_role),
        "resolutionNote": row.try_get::<Option<String>, _>("resolution_note")?,
        "requesterName": row.try_get::<Option<String>, _>("requester_name")?,
        "requesterEmail": row.try_get::<Option<String>, _>("requester_email")?,
        "createdAt": created_at.to_rfc3339(),
        "updatedAt": updated_at.to_rfc3339(),
    }))
}

async fn categories(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let available = available_roles(&state, &principal.student.tenant_id).await?;
    let items: Vec<Value> = CATEGORIES
        .iter()
        .map(|category| {
            json!({
                "key": category.key,
                "label": category.label,
                "handledBy": handled_by(assigned_role(category, &available)),
            })
        })
        .collect();
    Ok(Json(ApiResponse::new(Value::Array(items))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTicketRequest {
    category: String,
    subject: String,
    message: String,
    context: Option<Value>,
}

struct ValidTicket {
    category: &'static Category,
    subject: String,
    message: String,
    context: Value,
}

fn validate_ticket(input: CreateTicketRequest) -> ApiResult<ValidTicket> {
    let category = category(input.category.trim())
        .ok_or_else(|| ApiError::BadRequest("Choose what your request is about.".into()))?;
    let subject = input.subject.trim().to_owned();
    if !SUBJECT_CHARS.contains(&subject.chars().count()) {
        return Err(ApiError::BadRequest(
            "Give your request a short subject of 3 to 120 characters.".into(),
        ));
    }
    let message = input.message.trim().to_owned();
    if !MESSAGE_CHARS.contains(&message.chars().count()) {
        return Err(ApiError::BadRequest(
            "Describe the problem in 10 to 2,000 characters.".into(),
        ));
    }
    let context = match input.context {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value,
        Some(_) => {
            return Err(ApiError::BadRequest(
                "The request details couldn't be read. Try again.".into(),
            ));
        }
    };
    Ok(ValidTicket {
        category,
        subject,
        message,
        context,
    })
}

async fn create_ticket(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Json(input): Json<CreateTicketRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    let ticket = validate_ticket(input)?;
    let available = available_roles(&state, &principal.student.tenant_id).await?;
    let role = assigned_role(ticket.category, &available);
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let row = sqlx::query(&format!(
        r#"INSERT INTO campus_ops.support_tickets
             (tenant_id,requester_user_id,requester_name,requester_email,requester_role,
              category,subject,message,context,assigned_role)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           RETURNING {TICKET_COLUMNS}"#
    ))
    .bind(tenant)
    .bind(&principal.student.id)
    .bind(&principal.student.name)
    .bind(&principal.student.email)
    .bind(principal.roles.first())
    .bind(ticket.category.key)
    .bind(&ticket.subject)
    .bind(&ticket.message)
    .bind(&ticket.context)
    .bind(role)
    .fetch_one(&mut *tx)
    .await?;
    let value = ticket_json(&row)?;
    let ticket_id: Uuid = row.try_get("id")?;
    notify_tx(
        &mut tx,
        tenant,
        None,
        Some(role),
        "support",
        &format!("New help request: {}", ticket.subject),
        &format!(
            "{} asked for help with {}.",
            principal.student.name,
            ticket.category.label.to_lowercase()
        ),
        &json!({"id": ticket_id, "ticketId": ticket_id, "status": "open",
                "category": ticket.category.key}),
    )
    .await?;
    tx.commit().await?;

    if ticket.category.key == "app" {
        email_support_team(&state, &principal, &ticket, ticket_id);
    }
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

/// Best effort: a mail failure never fails the request, which is already stored
/// and in the admin inbox.
fn email_support_team(
    state: &AppState,
    principal: &AuthPrincipal,
    ticket: &ValidTicket,
    ticket_id: Uuid,
) {
    let mailer = state.mailer();
    if mailer.transport() == "disabled" {
        return;
    }
    let message = supercampus_notifications::EmailMessage {
        to: SUPPORT_EMAIL.into(),
        subject: format!("[SuperCampus help] {}", ticket.subject),
        text_body: format!(
            "Help request {ticket_id}\n\nInstitution: {}\nFrom: {} <{}>\nRole: {}\nCategory: {}\n\n{}\n\nContext: {}\n",
            principal.student.tenant_id,
            principal.student.name,
            principal.student.email,
            principal.roles.first().map_or("unassigned", String::as_str),
            ticket.category.label,
            ticket.message,
            ticket.context,
        ),
        html_body: None,
    };
    tokio::spawn(async move {
        if let Err(error) = mailer.send(message).await {
            tracing::warn!(error = ?error, %ticket_id, "failed to email the help request to support");
        }
    });
}

async fn own_tickets(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows = sqlx::query(&format!(
        r#"SELECT {TICKET_COLUMNS} FROM campus_ops.support_tickets
           WHERE tenant_id=$1 AND requester_user_id=$2
           ORDER BY created_at DESC LIMIT 200"#
    ))
    .bind(tenant)
    .bind(&principal.student.id)
    .fetch_all(db.pool())
    .await?;
    let tickets = rows
        .iter()
        .map(ticket_json)
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(ApiResponse::new(Value::Array(tickets))))
}

#[derive(Deserialize)]
struct InboxQuery {
    status: Option<String>,
}

fn is_ticket_status(status: &str) -> bool {
    matches!(status, "open" | "in_progress" | "resolved" | "closed")
}

async fn inbox(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Query(query): Query<InboxQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let status = query
        .status
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(status) = &status
        && !is_ticket_status(status)
    {
        return Err(ApiError::BadRequest(
            "Filter by open, in_progress, resolved or closed.".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows = sqlx::query(&format!(
        r#"SELECT {TICKET_COLUMNS} FROM campus_ops.support_tickets
           WHERE tenant_id=$1 AND ($2 OR assigned_role = ANY($3))
             AND ($4::text IS NULL OR status=$4)
           ORDER BY created_at DESC LIMIT 200"#
    ))
    .bind(tenant)
    .bind(is_admin(&principal))
    .bind(&principal.roles)
    .bind(&status)
    .fetch_all(db.pool())
    .await?;
    let tickets = rows
        .iter()
        .map(ticket_json)
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(ApiResponse::new(Value::Array(tickets))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateTicketRequest {
    status: String,
    note: Option<String>,
}

fn validate_update(input: &UpdateTicketRequest) -> ApiResult<Option<String>> {
    if !matches!(input.status.as_str(), "in_progress" | "resolved" | "closed") {
        return Err(ApiError::BadRequest(
            "Set the status to in progress, resolved or closed.".into(),
        ));
    }
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_owned);
    if note
        .as_deref()
        .is_some_and(|note| note.chars().count() > NOTE_MAX_CHARS)
    {
        return Err(ApiError::BadRequest(
            "Keep the note to 2,000 characters or fewer.".into(),
        ));
    }
    Ok(note)
}

async fn update_ticket(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Path(ticket_id): Path<Uuid>,
    Json(input): Json<UpdateTicketRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let note = validate_update(&input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let current = sqlx::query_as::<_, (String, String, Option<String>)>(
        r#"SELECT assigned_role, requester_user_id, requester_email
           FROM campus_ops.support_tickets WHERE id=$1 AND tenant_id=$2 FOR UPDATE"#,
    )
    .bind(ticket_id)
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("That help request doesn't exist.".into()))?;
    let (role, requester_user_id, requester_email) = current;
    if !is_admin(&principal) && !principal.roles.contains(&role) {
        return Err(ApiError::ForbiddenWithMessage(format!(
            "Only the {} can update this help request.",
            handled_by(&role).to_lowercase()
        )));
    }
    let row = sqlx::query(&format!(
        r#"UPDATE campus_ops.support_tickets
           SET status=$3, resolution_note=COALESCE($4,resolution_note), updated_at=now()
           WHERE id=$1 AND tenant_id=$2
           RETURNING {TICKET_COLUMNS}"#
    ))
    .bind(ticket_id)
    .bind(tenant)
    .bind(&input.status)
    .bind(&note)
    .fetch_one(&mut *tx)
    .await?;
    let value = ticket_json(&row)?;
    // Notifications are read against the tenant's own identity row, which is
    // matched by email; fall back to the id the ticket was raised under.
    let recipient = sqlx::query_scalar::<_, String>(
        r#"SELECT COALESCE(
             (SELECT id::text FROM identity.users WHERE lower(email)=lower($1) LIMIT 1), $2)"#,
    )
    .bind(&requester_email)
    .bind(&requester_user_id)
    .fetch_one(&mut *tx)
    .await?;
    let subject: String = row.try_get("subject")?;
    let body = match &note {
        Some(note) => format!("{subject}: {note}"),
        None => subject,
    };
    notify_tx(
        &mut tx,
        tenant,
        Some(&recipient),
        None,
        "support",
        &format!(
            "Your help request was updated: {}",
            status_label(&input.status)
        ),
        &body,
        &json!({"id": ticket_id, "ticketId": ticket_id, "status": input.status}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    fn routed(key: &str, available: &[&str]) -> &'static str {
        assigned_role(category(key).expect("known category"), &roles(available))
    }

    #[test]
    fn categories_route_to_the_role_that_handles_them() {
        let mec = [
            "accountant",
            "warden",
            "librarian",
            "hod",
            "owner",
            "tenant_admin",
        ];
        assert_eq!(routed("fees", &mec), "accountant");
        assert_eq!(routed("hostel", &mec), "warden");
        assert_eq!(routed("gatepass", &mec), "warden");
        assert_eq!(routed("library", &mec), "librarian");
        assert_eq!(routed("academics", &mec), "hod");
        assert_eq!(routed("canteen", &mec), "owner");
        assert_eq!(routed("app", &mec), "tenant_admin");
        assert_eq!(routed("other", &mec), "tenant_admin");
    }

    #[test]
    fn missing_roles_fall_back_to_the_admin_office() {
        assert_eq!(routed("fees", &["tenant_admin"]), "tenant_admin");
        assert_eq!(routed("library", &[]), "tenant_admin");
        assert_eq!(routed("canteen", &["accountant"]), "accountant");
        assert_eq!(routed("canteen", &[]), "tenant_admin");
    }

    #[test]
    fn every_category_says_who_handles_it() {
        assert_eq!(handled_by("accountant"), "Accounts office");
        assert_eq!(handled_by("warden"), "Hostel warden");
        assert_eq!(handled_by("tenant_admin"), "College admin office");
        assert!(category("unknown").is_none());
        assert_eq!(category_label("canteen"), "Canteen, stationery & laundry");
    }

    fn request(category: &str, subject: &str, message: &str) -> CreateTicketRequest {
        CreateTicketRequest {
            category: category.into(),
            subject: subject.into(),
            message: message.into(),
            context: None,
        }
    }

    #[test]
    fn ticket_validation_enforces_lengths_and_known_categories() {
        assert!(validate_ticket(request("fees", "Fee receipt", "My receipt is missing.")).is_ok());
        assert!(matches!(
            validate_ticket(request("parking", "Fee receipt", "My receipt is missing.")),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            validate_ticket(request("fees", "  a ", "My receipt is missing.")),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            validate_ticket(request("fees", "Fee receipt", "too short")),
            Err(ApiError::BadRequest(_))
        ));
        let mut with_array = request("fees", "Fee receipt", "My receipt is missing.");
        with_array.context = Some(json!([1, 2]));
        assert!(matches!(
            validate_ticket(with_array),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn staff_can_only_move_a_ticket_forward_to_known_states() {
        let update = |status: &str| UpdateTicketRequest {
            status: status.into(),
            note: Some("  ".into()),
        };
        assert_eq!(validate_update(&update("resolved")).expect("valid"), None);
        assert!(validate_update(&update("open")).is_err());
        assert!(validate_update(&update("done")).is_err());
    }
}
