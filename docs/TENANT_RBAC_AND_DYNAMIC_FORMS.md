# Tenant-managed RBAC and dynamic CRM forms

SuperCampus does not compile institution-specific CRM roles into Rust. Roles,
permission grants, user assignments, and form schemas are tenant data in
PostgreSQL. The only protected bootstrap role is `tenant_admin`; it receives
the wildcard permission `*` so an institution cannot lock itself out.

An unknown role has no permissions. There is no Applicant-tier fallback.

## Runtime authorization model

| Table | Purpose |
|---|---|
| `authz.permission_templates` | Platform capability catalog used for tenant bootstrap |
| `authz.permission_definitions` | Tenant-visible permission definitions |
| `authz.roles` | Tenant-created roles |
| `authz.role_permissions` | Grants with `all`, `assigned`, or `own` scope |
| `authz.user_roles` | Tenant user-to-role assignments |
| `identity.tenant_memberships` | Login membership and compatibility role projection |

The access JWT identifies the user, tenant, and session. On every protected
request the API reloads active roles and grants from these tables. An admin
change therefore takes effect on the next request without token refresh,
frontend rebuild, or deployment.

CRM middleware overwrites client-supplied access headers with verified values:

- `x-user-roles`: JSON array of active tenant role keys.
- `x-user-permissions`: JSON array of effective permission keys.
- `x-permission-scopes`: JSON object keyed by permission.
- `x-user-role`: first active role, retained for audit compatibility.

## Tenant-admin API

All endpoints require an access JWT or `sc_access` cookie. The caller must
have the listed permission; `tenant_admin` satisfies every check through
`*`.

| Method | Endpoint | Required permission | Result |
|---|---|---|---|
| GET | `/api/v1/authorization/permissions` | `authorization.permissions.read` | Permission catalog |
| GET | `/api/v1/authorization/roles` | `authorization.roles.read` | Roles, grants, and scopes |
| POST | `/api/v1/authorization/roles` | `authorization.roles.create` | Create an empty role |
| PUT | `/api/v1/authorization/roles/{roleId}` | `authorization.roles.update` | Update a role |
| DELETE | `/api/v1/authorization/roles/{roleId}` | `authorization.roles.delete` | Delete a non-protected role |
| PUT | `/api/v1/authorization/roles/{roleId}/permissions` | `authorization.roles.update` | Replace grants |
| GET | `/api/v1/authorization/users` | `authorization.users.read` | Tenant users and roles |
| POST | `/api/v1/authorization/users` | `authorization.users.create` | Create or join a user; returns `409` if already in the tenant |
| PUT | `/api/v1/authorization/users/{userId}/roles` | `authorization.users.update` | Replace user roles |

User email is globally normalized and unique. The same identity may join
different tenants, but a second create request for an existing membership is
rejected with `409 conflict`; callers must update the existing user's role
assignment instead.

Create a role:

```json
{
  "key": "crm_counselor",
  "name": "CRM Counselor",
  "team": "Admissions",
  "scope": "Works assigned enquiries"
}
```

Replace its permissions:

```json
{
  "permissions": [
    { "key": "crm.leads.read", "scope": "assigned", "constraints": {} },
    { "key": "crm.leads.update", "scope": "assigned", "constraints": {} },
    { "key": "crm.communications.send", "scope": "assigned", "constraints": {} }
  ]
}
```

The replacement is transactional. Omitting a previous grant revokes it.
Protected roles cannot be modified or deleted.

### Dynamic CRUD matrix

The Settings access-control screen does not contain a compiled role-to-action
table. It loads `authz.permission_definitions` through
`GET /api/v1/authorization/permissions` and groups each active definition by
`moduleKey`, `featureKey`, and its `crudActions` metadata. `crudActions` is an
array because one enforced permission can back multiple API operations. For
example, an upsert permission can authorize both Create and Update:

- `create` renders in the **C** cell;
- `read` renders in the **R** cell;
- `update` renders in the **U** cell;
- `delete` renders in the **D** cell.

The mapping lives in tenant permission metadata rather than frontend code. A
cell displays `N/A` when the feature has no corresponding API operation; for
example, a dashboard can be read but cannot be deleted. `N/A` is not a failed
permission and is not sent as a grant.

A cell may represent multiple atomic permission keys. For example, one feature
can expose several update operations. Enabling that cell grants every active
key represented by the cell; disabling it revokes those keys. The frontend
sends the complete role grant set to the transactional role-permission endpoint.
The backend rejects blank, duplicate, inactive, and cross-tenant permission
keys instead of silently accepting them.

Step 4 lists current tenant users and persists role assignment with
`PUT /api/v1/authorization/users/{userId}/roles`. A user receives the union of
all permissions from all assigned active roles. Because middleware reloads that
union on every protected request, a tenant-admin change is enforced on the
user's next API request. Reloading the frontend also refreshes visible controls
from `/api/auth/me`.

### Permission-driven frontend runtime

The staff frontend reads the effective permission array returned by
`/api/auth/me`; it does not infer access from a hardcoded role name. The same
permission helpers control:

- sidebar module visibility;
- Settings tab visibility;
- lead create and bulk-import actions;
- campaign creation;
- user/role creation and assignment controls;
- form create, edit, publish, and unpublish controls;
- dashboard capability indicators.

The frontend does not request authorization, forms, lead-board, or dashboard
resources when the effective permission set cannot read them. This prevents
avoidable `403` responses while the backend remains the final enforcement
boundary. If the user is authenticated but has no readable workspace, the UI
shows an explicit no-access state instead of exposing a default dashboard.

The centralized policy is in `apps/platform/src/lib/staff-access.ts` in the
frontend repository. No frontend rebuild is needed after an administrator
changes a role: `/api/auth/me` and the next protected request reload current
effective grants from the database.

Create a user:

```json
{
  "name": "Asha Rao",
  "email": "asha@example.edu",
  "password": "administrator-chosen-password",
  "roleIds": ["<tenant-role-uuid>"]
}
```

The administrator must supply a password between 12 characters and 72 bytes.
The API never returns that password. Passwords are hashed with PostgreSQL
`pgcrypto` before the user is committed.

## Dynamic lead-capture forms

Form definitions live in `crm.forms`. The JSON schema, status, and version
are tenant scoped. Submissions live in `crm.form_submissions`.

| Method | Endpoint | Permission | Purpose |
|---|---|---|---|
| GET | `/api/v1/crm/forms` | `crm.forms.read` | List forms |
| POST | `/api/v1/crm/forms` | `crm.forms.manage` | Create a draft |
| PUT | `/api/v1/crm/forms/{id}` | `crm.forms.manage` | Save name, form type, metadata, and a new draft version |
| POST | `/api/v1/crm/forms/{id}/publish` | `crm.forms.publish` | Publish |
| POST | `/api/v1/crm/forms/{id}/unpublish` | `crm.forms.publish` | Return to draft |
| GET | `/api/v1/crm/forms/published/lead-capture` | Forms read or leads create | Resolve published schema |
| POST | `/api/v1/crm/forms/{id}/submit` | Forms submit/manage or published lead form | Validate and submit |

The frontend builder stores fields in a `sections` array. Every field has a
stable `key`, display `label`, `type`, and `required` flag. A field can also
store `placeholder`, `helpText`, `width`, and tenant-managed `options`.
Dropdown, multi-select, and radio options are stored with the form version,
rendered by the frontend, and enforced by the backend during submission. The
publish endpoint rejects any choice field that has no configured options.
Form metadata stores the destination module, owner, and description/usage.

Example choice field:

```json
{
  "key": "course_type",
  "label": "Course type",
  "type": "Dropdown",
  "required": true,
  "placeholder": "Select a course",
  "helpText": "Courses currently accepting applications",
  "options": ["B.Tech Computer Science", "B.Tech Electronics", "MBA"]
}
```

After a `lead_capture` form is published, Create Lead resolves this schema
and renders it without a frontend rebuild. Submission validates required
fields. When no lead ID is supplied, CRM creates the lead and returns
`createdLeadId` while preserving the exact submitted values and form version.

In Settings -> Form Builders, use **Edit** to change the form name,
description, module, purpose, and owner. Use the pencil on a field to edit its
label, placeholder, help text, required state, width, and choices (one choice
per line). For CRM Create Lead, select module `CRM` and type `lead_capture`,
then use **Save draft** or **Publish**. A published Admissions/Application form
is live for Admissions and intentionally does not appear in CRM Create Lead.
Editing a live form creates a new draft version; **Save & republish** makes
that version active. The selected builder can also be unpublished from the
main builder toolbar.

## Frontend integration

The existing Users & Roles and Settings pages use:

- `apps/platform/src/lib/authorization-api.ts`
- `apps/platform/src/lib/crm-api.ts`
- `apps/platform/src/app/(staff)/dashboard/admissions/page.tsx`

These pages no longer seed a compiled CRM role list or lead-capture form. The
tenant database response is the source of truth.
