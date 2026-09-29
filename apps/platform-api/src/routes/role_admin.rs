//! Role management rules for the Admin Console that sit on top of the
//! existing role endpoints: which roles may be edited or deleted, member
//! counts, reassigning members before a delete, and which permissions a
//! tenant administrator may put on a role.

use axum::{
    Extension,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use super::{admin_users, require_effective_permission};
use crate::{
    error::{ApiError, ApiResult},
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

/// Roles the platform's own code relies on by key. They can be edited but
/// never deleted.
pub(super) fn is_system_role_key(role_key: &str) -> bool {
    matches!(
        role_key.trim().to_ascii_lowercase().as_str(),
        "tenant_admin" | "student" | "staff" | "parent" | "guardian"
    )
}

/// The first permission a tenant administrator may not put on a role: the
/// `*` wildcard (full tenant administration) and the platform's own
/// permissions stay with platform administrators.
pub(super) fn forbidden_permission_grant<'a>(
    keys: impl IntoIterator<Item = &'a str>,
    actor_is_platform_admin: bool,
) -> Option<&'a str> {
    if actor_is_platform_admin {
        return None;
    }
    keys.into_iter()
        .find(|key| *key == "*" || key.starts_with("platform.") || *key == "platform")
}

pub(super) struct RoleFacts {
    pub key: String,
    pub name: String,
    pub protected: bool,
    pub active: bool,
    pub member_count: i64,
}

pub(super) async fn role_facts(
    state: &AppState,
    tenant_slug: &str,
    role_id: Uuid,
) -> ApiResult<RoleFacts> {
    let database = state.database().ok_or_else(|| {
        ApiError::ServiceUnavailable("PostgreSQL is required for role management".into())
    })?;
    let row = sqlx::query(
        r#"SELECT role.role_key, role.name, role.protected, role.active,
                  (SELECT count(DISTINCT user_role.user_id)
                   FROM authz.user_roles user_role
                   JOIN identity.tenant_memberships membership
                     ON membership.tenant_id = user_role.tenant_id
                    AND membership.user_id = user_role.user_id
                   WHERE user_role.tenant_id = role.tenant_id
                     AND user_role.role_id = role.id) AS member_count
           FROM authz.roles role
           JOIN platform.tenants tenant ON tenant.id = role.tenant_id
           WHERE tenant.slug = $1 AND role.id = $2"#,
    )
    .bind(tenant_slug)
    .bind(role_id)
    .fetch_optional(database.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("role not found".into()))?;
    Ok(RoleFacts {
        key: row.try_get("role_key")?,
        name: row.try_get("name")?,
        protected: row.try_get("protected")?,
        active: row.try_get("active")?,
        member_count: row.try_get("member_count")?,
    })
}

/// Member count per role id for the tenant (members of the tenant only).
pub(super) async fn member_counts(
    state: &AppState,
    tenant_slug: &str,
) -> ApiResult<std::collections::HashMap<Uuid, i64>> {
    let database = state.database().ok_or_else(|| {
        ApiError::ServiceUnavailable("PostgreSQL is required for role management".into())
    })?;
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(
        r#"SELECT user_role.role_id, count(DISTINCT user_role.user_id)
           FROM authz.user_roles user_role
           JOIN platform.tenants tenant ON tenant.id = user_role.tenant_id
           JOIN identity.tenant_memberships membership
             ON membership.tenant_id = user_role.tenant_id
            AND membership.user_id = user_role.user_id
           WHERE tenant.slug = $1
           GROUP BY user_role.role_id"#,
    )
    .bind(tenant_slug)
    .fetch_all(database.pool())
    .await?;
    Ok(rows.into_iter().collect())
}

/// Refuses changes to a protected role with a clear 409 instead of the
/// storage layer's generic failure.
pub(super) async fn ensure_role_editable(
    state: &AppState,
    tenant_slug: &str,
    role_id: Uuid,
) -> ApiResult<RoleFacts> {
    let facts = role_facts(state, tenant_slug, role_id).await?;
    if facts.protected {
        return Err(ApiError::Conflict(format!(
            "{} is a protected system role and cannot be changed",
            facts.name
        )));
    }
    Ok(facts)
}

/// Why a role cannot be deleted as requested, if it cannot.
pub(super) fn role_delete_refusal(
    facts: &RoleFacts,
    reassign_to: Option<Uuid>,
    role_id: Uuid,
) -> Option<String> {
    if facts.protected || is_system_role_key(&facts.key) {
        return Some(format!(
            "{} is a system role and cannot be deleted",
            facts.name
        ));
    }
    if reassign_to == Some(role_id) {
        return Some("Choose a different role to move members to".into());
    }
    if facts.member_count > 0 && reassign_to.is_none() {
        return Some(format!(
            "{} {} this role. Move them to another role first.",
            facts.member_count,
            if facts.member_count == 1 {
                "person has"
            } else {
                "people have"
            }
        ));
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeleteRoleQuery {
    reassign_to: Option<Uuid>,
}

/// `DELETE /authorization/roles/{role_id}[?reassignTo={role_id}]`
pub(super) async fn delete_role(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(role_id): Path<Uuid>,
    Query(query): Query<DeleteRoleQuery>,
) -> ApiResult<StatusCode> {
    require_effective_permission(&access, "authorization.roles.delete")?;
    let tenant_slug = principal.student.tenant_id.as_str();
    admin_users::guard_role_definition(&state, tenant_slug, &access, Some(role_id), None, None)
        .await?;
    let facts = role_facts(&state, tenant_slug, role_id).await?;
    if let Some(message) = role_delete_refusal(&facts, query.reassign_to, role_id) {
        return Err(ApiError::Conflict(message));
    }
    let database = state.database().ok_or_else(|| {
        ApiError::ServiceUnavailable("PostgreSQL is required for role management".into())
    })?;
    let tenant_id: Uuid = sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_one(database.pool())
        .await?;
    let mut transaction = database.pool().begin().await?;
    if let Some(target_id) = query.reassign_to.filter(|_| facts.member_count > 0) {
        let target = role_facts(&state, tenant_slug, target_id).await?;
        if !target.active {
            return Err(ApiError::Conflict(format!(
                "{} is inactive; choose an active role",
                target.name
            )));
        }
        // Moving members into a role is assigning it: the same escalation
        // guard as the user-role editor applies.
        admin_users::guard_role_assignment(&state, tenant_slug, &access, None, &[target_id])
            .await?;
        sqlx::query(
            r#"INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
               SELECT tenant_id, user_id, $3, $4 FROM authz.user_roles
               WHERE tenant_id = $1 AND role_id = $2
               ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(role_id)
        .bind(target_id)
        .bind(&principal.student.id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"UPDATE identity.tenant_memberships
               SET roles = array_append(array_remove(array_remove(roles, $2), $3), $3),
                   updated_at = now()
               WHERE tenant_id = $1 AND $2 = ANY(roles)"#,
        )
        .bind(tenant_id)
        .bind(&facts.key)
        .bind(&target.key)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        r#"UPDATE identity.tenant_memberships
           SET roles = array_remove(roles, $2), updated_at = now()
           WHERE tenant_id = $1 AND $2 = ANY(roles)"#,
    )
    .bind(tenant_id)
    .bind(&facts.key)
    .execute(&mut *transaction)
    .await?;
    // Grants, surfaces and user assignments cascade with the role.
    sqlx::query("DELETE FROM authz.roles WHERE tenant_id = $1 AND id = $2 AND NOT protected")
        .bind(tenant_id)
        .bind(role_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    state.forget_cached_identities().await;
    state.publish_realtime(RealtimePublication::tenant(
        principal.student.tenant_id.clone(),
        "authorization.changed",
        json!({"resource": "role", "action": "deleted", "roleId": role_id,
               "reassignedTo": query.reassign_to}),
    ));
    tracing::info!(
        %role_id,
        role_key = facts.key,
        reassigned_to = ?query.reassign_to,
        members = facts.member_count,
        actor = principal.student.id,
        tenant_slug,
        "tenant role deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(key: &str, protected: bool, members: i64) -> RoleFacts {
        RoleFacts {
            key: key.into(),
            name: key.into(),
            protected,
            active: true,
            member_count: members,
        }
    }

    #[test]
    fn system_and_protected_roles_are_never_deleted() {
        let id = Uuid::new_v4();
        assert!(role_delete_refusal(&facts("tenant_admin", true, 0), None, id).is_some());
        assert!(role_delete_refusal(&facts("student", false, 0), None, id).is_some());
        assert!(role_delete_refusal(&facts("custom", true, 0), None, id).is_some());
    }

    #[test]
    fn roles_with_members_need_a_reassignment() {
        let id = Uuid::new_v4();
        let refusal = role_delete_refusal(&facts("custom", false, 2), None, id).unwrap();
        assert!(refusal.starts_with("2 people have"));
        assert!(
            role_delete_refusal(&facts("custom", false, 2), Some(Uuid::new_v4()), id).is_none()
        );
        assert!(role_delete_refusal(&facts("custom", false, 2), Some(id), id).is_some());
        assert!(role_delete_refusal(&facts("custom", false, 0), None, id).is_none());
    }

    #[test]
    fn tenant_admins_cannot_grant_wildcard_or_platform_permissions() {
        assert_eq!(
            forbidden_permission_grant(["fees.x.read", "*"], false),
            Some("*")
        );
        assert_eq!(
            forbidden_permission_grant(["platform.configuration.update"], false),
            Some("platform.configuration.update")
        );
        assert_eq!(forbidden_permission_grant(["fees.x.read"], false), None);
        assert_eq!(forbidden_permission_grant(["*"], true), None);
    }
}
