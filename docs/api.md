# Local platform API

The local API connects to PostgreSQL through `DATABASE_URL` and runs the embedded
runtime migrations at startup. Tenant module records, configurations, hashed
sessions, and UI state persist across backend restarts. Tests continue to use the
isolated in-memory `AppState` unless a PostgreSQL integration test is explicitly enabled.

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
| GET | `/api/v1/modules` | All 12 domain modules |
| GET | `/api/v1/modules/{moduleKey}` | One module descriptor |

## Configuration and module records

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/configuration/{namespace}` | Active tenant configuration |
| PUT | `/api/v1/configuration/{namespace}` | Create the next configuration version |
| GET | `/api/v1/{moduleKey}/records` | Tenant-filtered module records |
| POST | `/api/v1/{moduleKey}/records` | Create a module record |
| GET | `/api/v1/{moduleKey}/records/{recordId}` | Read a module record |
| PATCH | `/api/v1/{moduleKey}/records/{recordId}` | Replace a record's dynamic data |
| DELETE | `/api/v1/{moduleKey}/records/{recordId}` | Delete a module record |

Pass `x-tenant-id`; local requests default to `tenant-local`.

## Frontend compatibility

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/auth/tenants` | Local tenant selector |
| POST | `/api/auth/login` | Create an HTTP-only local session |
| GET | `/api/auth/me` | Current student session |
| POST | `/api/auth/logout` | Revoke the local session |
| GET | `/api/state` | Current student UI state |
| PUT | `/api/state` | Save current student UI state |

Local credentials are `student@supercampus.local` and the value of
`DEV_LOGIN_PASSWORD` (`SuperCampus@123` by default). This development login must
be replaced by the `authn` provider before any shared or production deployment.