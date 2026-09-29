//! Permanent deletion of tenant users from the Admin Console.
//!
//! What "delete" does, and why:
//!
//! * The tenant membership, role assignments, direct grants, UI state, web and
//!   app sessions and password reset links are removed or revoked. The person
//!   disappears from the user list and can never sign in to this tenant again.
//! * The `identity.users` row is kept as a tombstone when the person belongs
//!   to no other tenant: it is disabled, its email is replaced with a unique
//!   `deleted+<id>@deleted.invalid` address (so the real address can be used
//!   for a new account), its password is replaced with an unusable hash and
//!   its profile is cleared. The display name stays as a history label.
//!   Wallet ledgers, canteen orders, gate passes, attendance, marks, loans and
//!   audit trails reference the user by id (many by text, without a foreign
//!   key); a hard delete would either be refused by foreign keys or leave
//!   those records pointing at nothing, and some cascade (marks, fee links),
//!   which would destroy financial and academic history.
//! * Student and employee master records are unlinked from the account and
//!   marked `deleted` / `terminated`, with email and phone removed. Their row
//!   ids and names stay so historical records keep resolving.

use std::collections::HashSet;

use axum::{Extension, Json, extract::Path, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{admin_users::may_manage_user, require_effective_permission};
use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub(super) const DELETE_PERMISSION: &str = "authorization.users.delete";
const MAX_BULK_DELETE: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BulkDeleteUsersRequest {
    ids: Vec<Uuid>,
}

/// `DELETE /authorization/users/{user_id}`
pub(super) async fn delete_tenant_user(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, DELETE_PERMISSION)?;
    let outcome = delete_users(&state, &principal, &access, &[user_id]).await?;
    if let Some(failure) = outcome.failed.first() {
        return Err(match failure.reason {
            FailureReason::NotFound => ApiError::NotFound("tenant user not found".into()),
            FailureReason::Privileged => ApiError::ForbiddenWithMessage(
                "Only a platform administrator can delete this account".into(),
            ),
        });
    }
    Ok(Json(ApiResponse::new(outcome.to_json())))
}

/// `POST /authorization/users/bulk-delete` with `{ "ids": [...] }`.
pub(super) async fn bulk_delete_tenant_users(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(request): Json<BulkDeleteUsersRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_effective_permission(&access, DELETE_PERMISSION)?;
    let ids = unique_ids(&request.ids);
    if ids.is_empty() {
        return Err(ApiError::BadRequest(
            "choose at least one user to delete".into(),
        ));
    }
    if ids.len() > MAX_BULK_DELETE {
        return Err(ApiError::BadRequest(format!(
            "delete at most {MAX_BULK_DELETE} users at a time"
        )));
    }
    let outcome = delete_users(&state, &principal, &access, &ids).await?;
    Ok(Json(ApiResponse::new(outcome.to_json())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureReason {
    NotFound,
    Privileged,
}

impl FailureReason {
    fn message(self) -> &'static str {
        match self {
            Self::NotFound => "Not a user of this campus",
            Self::Privileged => "Only a platform administrator can delete this account",
        }
    }
}

#[derive(Debug, Default)]
struct DeleteOutcome {
    deleted: Vec<Uuid>,
    failed: Vec<Failure>,
}

#[derive(Debug)]
struct Failure {
    id: Uuid,
    reason: FailureReason,
}

impl DeleteOutcome {
    fn to_json(&self) -> Value {
        json!({
            "deleted": self.deleted,
            "deletedCount": self.deleted.len(),
            "failed": self.failed.iter().map(|failure| json!({
                "id": failure.id,
                "reason": failure.reason.message(),
            })).collect::<Vec<_>>(),
            "sessionsRevoked": !self.deleted.is_empty(),
        })
    }
}

fn unique_ids(ids: &[Uuid]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// Whether deleting `targets` would leave the tenant without an active
/// administrator. `remaining_admins` counts administrators outside `targets`.
pub(super) fn removes_last_admin(targets_include_admin: bool, remaining_admins: i64) -> bool {
    targets_include_admin && remaining_admins == 0
}

/// Roles whose holders administer the tenant: the tenant administrator role
/// and any role granted the `*` wildcard.
const ADMIN_ROLE_PREDICATE: &str = r#"(role.role_key = 'tenant_admin' OR EXISTS (
        SELECT 1 FROM authz.role_permissions grant_row
        WHERE grant_row.tenant_id = role.tenant_id AND grant_row.role_id = role.id
          AND grant_row.permission_key = '*'))"#;

async fn delete_users(
    state: &AppState,
    principal: &AuthPrincipal,
    access: &EffectiveAccess,
    requested: &[Uuid],
) -> ApiResult<DeleteOutcome> {
    let tenant_slug = principal.student.tenant_id.as_str();
    let actor_id = principal.student.id.as_str();
    if Uuid::parse_str(actor_id).is_ok_and(|me| requested.contains(&me)) {
        return Err(ApiError::BadRequest(
            "You cannot delete your own account".into(),
        ));
    }
    let control = state.database().ok_or_else(|| {
        ApiError::ServiceUnavailable("PostgreSQL is required for user management".into())
    })?;
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_optional(control.pool())
        .await?
        .ok_or_else(|| ApiError::NotFound("tenant not found".into()))?;

    let mut outcome = DeleteOutcome::default();
    let members: Vec<(Uuid, String)> = sqlx::query_as(
        r#"SELECT membership.user_id, lower(account.email)
           FROM identity.tenant_memberships membership
           JOIN identity.users account ON account.id = membership.user_id
           WHERE membership.tenant_id = $1 AND membership.user_id = ANY($2)"#,
    )
    .bind(tenant_id)
    .bind(requested)
    .fetch_all(control.pool())
    .await?;
    let platform_admin = crate::platform_admin::is_platform_admin(access);
    let mut targets = Vec::new();
    let mut emails = Vec::new();
    for id in requested {
        let Some((_, email)) = members.iter().find(|(member, _)| member == id) else {
            outcome.failed.push(Failure {
                id: *id,
                reason: FailureReason::NotFound,
            });
            continue;
        };
        let roles = state.tenant_user_role_descriptors(tenant_slug, *id).await?;
        if !may_manage_user(&access.roles, platform_admin, &roles) {
            outcome.failed.push(Failure {
                id: *id,
                reason: FailureReason::Privileged,
            });
            continue;
        }
        targets.push(*id);
        emails.push(email.clone());
    }
    if targets.is_empty() {
        return Ok(outcome);
    }
    let target_texts: Vec<String> = targets.iter().map(Uuid::to_string).collect();

    // Stage the tenant database first, as the rename does; commit it after
    // the control plane so a failure there leaves the account intact.
    let mut tenant_transaction = match state.tenant_database(tenant_slug).await {
        Ok(tenant_db)
            if tenant_db.pool().connect_options().get_database()
                != control.pool().connect_options().get_database() =>
        {
            Some(tenant_db.pool().begin().await?)
        }
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, tenant_slug, "tenant database unavailable; deleting in the control plane only");
            None
        }
    };

    let mut transaction = control.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('tenant-user-delete:' || $1::text))")
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await?;
    let targets_include_admin: bool = sqlx::query_scalar(&format!(
        r#"SELECT EXISTS (
               SELECT 1 FROM authz.user_roles user_role
               JOIN authz.roles role
                 ON role.id = user_role.role_id AND role.tenant_id = user_role.tenant_id
               WHERE user_role.tenant_id = $1 AND user_role.user_id = ANY($2)
                 AND {ADMIN_ROLE_PREDICATE})"#
    ))
    .bind(tenant_id)
    .bind(&targets)
    .fetch_one(&mut *transaction)
    .await?;
    let remaining_admins: i64 = sqlx::query_scalar(&format!(
        r#"SELECT count(DISTINCT membership.user_id)
           FROM identity.tenant_memberships membership
           JOIN identity.users account ON account.id = membership.user_id AND account.active
           JOIN authz.user_roles user_role
             ON user_role.tenant_id = membership.tenant_id
            AND user_role.user_id = membership.user_id
           JOIN authz.roles role
             ON role.id = user_role.role_id AND role.tenant_id = user_role.tenant_id
            AND role.active
           WHERE membership.tenant_id = $1 AND membership.active
             AND NOT (membership.user_id = ANY($2))
             AND {ADMIN_ROLE_PREDICATE}"#
    ))
    .bind(tenant_id)
    .bind(&targets)
    .fetch_one(&mut *transaction)
    .await?;
    if removes_last_admin(targets_include_admin, remaining_admins) {
        return Err(ApiError::Conflict(
            "This would remove the last campus administrator. Make someone else an administrator first."
                .into(),
        ));
    }

    remove_access(&mut transaction, tenant_id, &targets, &target_texts).await?;
    // Only accounts that now belong to no tenant at all are tombstoned; a
    // person who is also a member elsewhere just loses this membership.
    let orphaned: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT account.id FROM identity.users account
           WHERE account.id = ANY($1)
             AND NOT EXISTS (
                 SELECT 1 FROM identity.tenant_memberships membership
                 WHERE membership.user_id = account.id
             )"#,
    )
    .bind(&targets)
    .fetch_all(&mut *transaction)
    .await?;
    tombstone_accounts(&mut transaction, &orphaned, actor_id, tenant_slug).await?;
    scrub_person_records(&mut transaction, tenant_slug, &targets, &emails).await?;
    if let Some(tenant_tx) = tenant_transaction.as_mut() {
        let replica_tenant: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug = $1")
                .bind(tenant_slug)
                .fetch_optional(&mut **tenant_tx)
                .await?;
        if let Some(replica_tenant) = replica_tenant {
            remove_access(tenant_tx, replica_tenant, &targets, &target_texts).await?;
            tombstone_accounts(tenant_tx, &orphaned, actor_id, tenant_slug).await?;
            scrub_person_records(tenant_tx, tenant_slug, &targets, &emails).await?;
        }
    }
    transaction.commit().await?;
    if let Some(tenant_tx) = tenant_transaction
        && let Err(error) = tenant_tx.commit().await
    {
        tracing::error!(%error, tenant_slug, "tenant commit failed after users were deleted in the control plane");
    }
    state.forget_cached_identities().await;
    for id in &targets {
        state.publish_realtime(
            RealtimePublication::tenant(
                tenant_slug.to_owned(),
                "identity.user.deleted",
                json!({"userId": id}),
            )
            .for_user(id.to_string()),
        );
    }
    tracing::warn!(
        actor_id,
        tenant_slug,
        count = targets.len(),
        users = ?targets,
        "tenant administrator permanently deleted users"
    );
    outcome.deleted = targets;
    Ok(outcome)
}

/// Runs `statement` inside a savepoint and ignores "table does not exist", so
/// the same cleanup works on control and tenant databases whose optional
/// schemas differ.
async fn execute_optional<'q>(
    transaction: &mut Transaction<'static, Postgres>,
    statement: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
) -> ApiResult<()> {
    let mut savepoint = sqlx::Acquire::begin(&mut **transaction).await?;
    match statement.execute(&mut *savepoint).await {
        Ok(_) => {
            savepoint.commit().await?;
            Ok(())
        }
        Err(sqlx::Error::Database(error))
            if matches!(error.code().as_deref(), Some("42P01") | Some("42703")) =>
        {
            savepoint.rollback().await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn remove_access(
    transaction: &mut Transaction<'static, Postgres>,
    tenant_id: Uuid,
    targets: &[Uuid],
    target_texts: &[String],
) -> ApiResult<()> {
    execute_optional(
        transaction,
        sqlx::query("DELETE FROM authz.user_roles WHERE tenant_id = $1 AND user_id = ANY($2)")
            .bind(tenant_id)
            .bind(targets),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            "DELETE FROM authz.assignments WHERE tenant_id = $1 AND principal_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(targets),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE identity.auth_sessions SET revoked_at = now()
               WHERE tenant_id = $1 AND user_id = ANY($2) AND revoked_at IS NULL"#,
        )
        .bind(tenant_id)
        .bind(target_texts),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            "DELETE FROM identity.local_sessions WHERE tenant_id = $1 AND user_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(target_texts),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query("DELETE FROM identity.ui_states WHERE tenant_id = $1 AND user_id = ANY($2)")
            .bind(tenant_id)
            .bind(target_texts),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE campus_ops.shop_user_assignments SET is_active = false, updated_at = now()
               WHERE tenant_id = $1 AND user_id = ANY($2)"#,
        )
        .bind(tenant_id)
        .bind(target_texts),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            "DELETE FROM identity.tenant_memberships WHERE tenant_id = $1 AND user_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(targets),
    )
    .await?;
    Ok(())
}

/// Disables accounts that no longer belong to any tenant, keeping the row as
/// a history label (see the module documentation).
async fn tombstone_accounts(
    transaction: &mut Transaction<'static, Postgres>,
    targets: &[Uuid],
    actor_id: &str,
    tenant_slug: &str,
) -> ApiResult<()> {
    if targets.is_empty() {
        return Ok(());
    }
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE identity.users account
               SET active = false,
                   email = 'deleted+' || account.id::text || '@deleted.invalid',
                   password_hash = crypt(gen_random_uuid()::text, gen_salt('bf', 6)),
                   profile = jsonb_build_object(
                       'deleted', true, 'deletedAt', now(),
                       'deletedBy', $2::text, 'deletedFromTenant', $3::text),
                   updated_at = now()
               WHERE account.id = ANY($1)"#,
        )
        .bind(targets)
        .bind(actor_id)
        .bind(tenant_slug),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            r#"DELETE FROM identity.password_reset_tokens token
               WHERE token.user_id = ANY($1)"#,
        )
        .bind(targets),
    )
    .await?;
    Ok(())
}

/// Unlinks the tenant's student, employee and guardian records from the
/// deleted accounts and strips contact details, keeping ids and names for
/// history.
async fn scrub_person_records(
    transaction: &mut Transaction<'static, Postgres>,
    tenant_slug: &str,
    targets: &[Uuid],
    emails: &[String],
) -> ApiResult<()> {
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE core.students student
               SET status = 'deleted', user_account_id = NULL, email = NULL, phone = NULL,
                   profile = COALESCE(student.profile, '{}'::jsonb)
                       - 'email' - 'phone' - 'mobile' - 'mobileNumber' - 'address',
                   updated_at = now()
               FROM platform.tenants tenant
               WHERE tenant.id = student.tenant_id AND tenant.slug = $1
                 AND (student.user_account_id = ANY($2) OR lower(student.email) = ANY($3))"#,
        )
        .bind(tenant_slug)
        .bind(targets)
        .bind(emails),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE core.employees employee
               SET status = 'terminated', user_id = NULL, email = NULL, phone = NULL,
                   profile = COALESCE(employee.profile, '{}'::jsonb)
                       - 'email' - 'phone' - 'mobile' - 'mobileNumber' - 'address',
                   updated_at = now()
               FROM platform.tenants tenant
               WHERE tenant.id = employee.tenant_id AND tenant.slug = $1
                 AND (employee.user_id = ANY($2) OR lower(employee.email) = ANY($3))"#,
        )
        .bind(tenant_slug)
        .bind(targets)
        .bind(emails),
    )
    .await?;
    execute_optional(
        transaction,
        sqlx::query(
            r#"UPDATE core.guardians guardian
               SET user_id = NULL, updated_at = now()
               FROM platform.tenants tenant
               WHERE tenant.id = guardian.tenant_id AND tenant.slug = $1
                 AND guardian.user_id = ANY($2)"#,
        )
        .bind(tenant_slug)
        .bind(targets),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_remove_the_last_administrator() {
        assert!(removes_last_admin(true, 0));
        assert!(!removes_last_admin(true, 1));
        assert!(!removes_last_admin(false, 0));
    }

    #[test]
    fn bulk_ids_are_deduplicated_in_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(unique_ids(&[a, b, a]), vec![a, b]);
    }

    #[test]
    fn bulk_request_rejects_unknown_fields() {
        let id = Uuid::new_v4();
        let request: BulkDeleteUsersRequest =
            serde_json::from_str(&format!(r#"{{"ids":["{id}"]}}"#)).unwrap();
        assert_eq!(request.ids, vec![id]);
        assert!(serde_json::from_str::<BulkDeleteUsersRequest>(r#"{"userIds":[]}"#).is_err());
    }

    #[test]
    fn failure_reasons_read_as_sentences() {
        assert!(FailureReason::NotFound.message().starts_with("Not a user"));
        assert!(
            FailureReason::Privileged
                .message()
                .contains("platform administrator")
        );
    }
}
