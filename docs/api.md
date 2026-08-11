# Local platform API

The API connects to the control plane through CONTROL_DATABASE_URL. The tenant database registry resolves each signed institution context to an independently migrated PostgreSQL database. Refresh sessions and dynamic RBAC remain in the control plane; CRM, module records, configuration, and UI state persist in the institution database. Tests continue to use isolated in-memory state unless a PostgreSQL integration test is explicitly enabled.

`GET /ready` performs a live database query and reports `storage: postgresql`.
Keep the real connection string only in the ignored `.env`; never commit it.

## System and discovery

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/health` | Liveness and build version |
| GET | `/ready` | Runtime/storage readiness |
| GET | `/api/v1/` | API index |
| GET | `/api/v1/bootstrap` | Tenant/user services, modules, and navigation |
| GET | `/api/v1/services` | Platform service catalog |
| GET | `/api/v1/services/{serviceKey}` | One service descriptor |
| GET | `/api/v1/modules` | Domain modules visible to the current user |
| GET | `/api/v1/modules/{moduleKey}` | One module descriptor |

`/api/v1/bootstrap`, `/api/v1/modules`, and `/api/v1/modules/{moduleKey}` are
filtered by the caller's effective permission set. Bootstrap also returns
`roles`, `permissions`, and `permissionScopes`; inaccessible modules are not
advertised to the client.

## CRM command center

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/crm/dashboard/operations` | Tenant/role-scoped CRM metrics, priority queue, automation state, source ROI, data quality, and cases |
| GET | `/api/v1/crm/campaigns` | Tenant campaign finance and attribution records |
| POST | `/api/v1/crm/campaigns` | Create or update campaign budget, spend, revenue, landing pages, UTM, and status |

The complete request/response contract and calculation definitions are in
[`CRM_API_ENDPOINTS.md`](CRM_API_ENDPOINTS.md).

## Tenant access management

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/authorization/permissions` | Tenant permission catalog |
| GET / POST | `/api/v1/authorization/roles` | List or create tenant roles |
| PUT / DELETE | `/api/v1/authorization/roles/{roleId}` | Update or remove a custom role |
| PUT | `/api/v1/authorization/roles/{roleId}/permissions` | Replace grants and scopes |
| GET / POST | `/api/v1/authorization/users` | List or create tenant users |
| PUT | `/api/v1/authorization/users/{userId}/roles` | Replace user-role assignments |

See [`TENANT_RBAC_AND_DYNAMIC_FORMS.md`](TENANT_RBAC_AND_DYNAMIC_FORMS.md)
for request bodies, database tables, enforcement, and dynamic forms.

## Configuration and module records

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/configuration/{namespace}` | Active tenant configuration |
| PUT | `/api/v1/configuration/{namespace}` | Create the next configuration version |
| GET | `/api/v1/workflows/{moduleKey}/{featureKey}` | Effective tenant workflow definition |
| POST | `/api/v1/workflows/{moduleKey}/{featureKey}/transitions/validate` | Validate a workflow action and return the next state |
| GET | `/api/v1/{moduleKey}/records` | Tenant-filtered module records |
| POST | `/api/v1/{moduleKey}/records` | Create a module record |
| GET | `/api/v1/{moduleKey}/records/{recordId}` | Read a module record |
| PATCH | `/api/v1/{moduleKey}/records/{recordId}` | Replace a record's dynamic data |
| DELETE | `/api/v1/{moduleKey}/records/{recordId}` | Delete a module record |

Runtime enforcement is independent for each operation:

- configuration read: `platform.configuration.read`
- configuration update: `platform.configuration.update`
- module record create: `{moduleKey}.records.create`
- module record read/list: `{moduleKey}.records.read`
- module record update: `{moduleKey}.records.update`
- module record delete: `{moduleKey}.records.delete`

Migration `0016_platform_runtime_permissions.sql` registers these permissions
for every current platform module. Roles receive none automatically; tenant
admins grant only the operations required by each institution role.

All `/api/v1/*` routes require `Authorization: Bearer <accessToken>` or the
HTTP-only `sc_access` JWT cookie. Tenant context comes from the verified `tid`
claim. If `x-tenant-id` is also sent, it must match that claim; a mismatch returns
`403 Forbidden`.

## Frontend compatibility

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/auth/login` | Create JWT access token and rotating refresh session |
| POST | `/api/auth/refresh` | Rotate refresh token and issue a new access JWT |
| GET | `/api/auth/me` | Current verified user/session |
| POST | `/api/auth/logout` | Revoke the server session and clear auth cookies |
| GET | `/api/state` | Current student UI state |
| PUT | `/api/state` | Save current student UI state |

Login accepts email and password. Institution membership is resolved by the control plane. The response contains
`accessToken`, `tokenType`, `expiresAt`, `sessionId`, `roles`, and `student`.
The same access JWT is set as HTTP-only `sc_access`; the opaque rotating refresh
token is set as HTTP-only `sc_session` and is restricted to `/api/auth`.

```http
POST /api/auth/login
Content-Type: application/json

{
  "email": "<issued-email>",
  "password": "<issued-password>"
}
```

```http
GET /api/v1/modules
Authorization: Bearer <accessToken>
```

Login verifies the stored password hash and active institution membership in the control database.
Optional test identities are seeded only when `SEED_TEST_USERS=true`; credentials
come from environment variables and plaintext passwords are never committed.
