# Platform API endpoints

Base URL: `http://localhost:4000`. Except where marked public, endpoints require bearer JWT or the `sc_access` cookie. Tenant/user context is derived by middleware.

## Endpoint inventory

| Method | Path | Purpose | Auth | Request | Success | Database/effects |
|---|---|---|---|---|---|---|
| GET | `/health` | Process liveness/version | No | None | 200 `HealthDocument` | None |
| GET | `/ready` | Runtime/storage readiness | No | None | 200 checks | `SELECT 1`; 500 on failure |
| GET | `/api/v1/` | API index/counts | Yes | None | 200 data object | None |
| GET | `/api/v1/bootstrap` | Tenant/user catalogs and navigation | Yes | None | 200 `BootstrapDocument` | None; built from static descriptors |
| GET | `/api/v1/services` | List service descriptors | Yes | None | 200 array | None |
| GET | `/api/v1/services/{service_key}` | Get service descriptor | Yes | path string | 200 object | None |
| GET | `/api/v1/modules` | List module descriptors | Yes | None | 200 array | None |
| GET | `/api/v1/modules/{module_key}` | Get module descriptor | Yes | path string | 200 object | None |
| GET | `/api/v1/configuration/{namespace}` | Read tenant JSON configuration | Yes | path string | 200 `ConfigurationDocument` | Read `configuration.runtime_documents` |
| PUT | `/api/v1/configuration/{namespace}` | Upsert/version configuration | Yes | `{ "value": any }` | 200 configuration | Upsert table; increment version |
| GET | `/api/v1/{module_key}/records` | List tenant dynamic records | Yes | path string | 200 array | Read `platform.dynamic_records` |
| POST | `/api/v1/{module_key}/records` | Create dynamic record | Yes | `CreateRecordRequest` | 201 record | Insert dynamic record |
| GET | `/api/v1/{module_key}/records/{record_id}` | Read one record | Yes | module string + UUID | 200 record | Tenant/module/id lookup |
| PATCH | `/api/v1/{module_key}/records/{record_id}` | Replace record `data` | Yes | `{ "data": any }` | 200 record | Update data and timestamp |
| DELETE | `/api/v1/{module_key}/records/{record_id}` | Delete one record | Yes | path params | 204 | Physical delete |
| GET | `/api/state` | Read current user UI state | Yes | None | 200 state/version/time | Read `identity.ui_states` |
| PUT | `/api/state` | Save current user UI state | Yes | `SaveAppStateRequest` | 200 state/version/time | Upsert and increment version |

Authentication endpoints are documented in [auth.md](auth.md), bringing the platform/auth total to 22 operations.

## Request schemas and validation

### CreateRecordRequest

```json
{
  "recordType": "application",
  "data": {}
}
```

`recordType` is required, must be a JSON string and cannot be blank. `data` is optional and defaults to JSON null. `module_key` must match a registered descriptor.

### UpdateRecordRequest

`data` is required and may contain any JSON value. Schema validation and optimistic concurrency are not implemented.

### PutConfigurationRequest

`value` is required and may contain any JSON value. `namespace` must be nonblank for PUT. Schema-level namespace authorization is not implemented.

### SaveAppStateRequest

```json
{
  "state": {},
  "action": "dashboard_updated"
}
```

`state` is required; `action` is optional. Per-field validation and size limits are not implemented.

## Responses and errors

Successful JSON uses `{ "data": ... }` except health/readiness. Deletes return no body. Possible application statuses are `200`, `201`, `204`, `400`, `401`, `403`, `404` and `500`. Invalid UUID/JSON extraction may be framework-generated `400`.

`GET service/module` returns 404 for unknown keys. Record operations return 404 for an unknown module or record. Configuration GET returns 404 when absent.

## Authorization limitation

These routes authenticate and isolate tenants, but do not enforce granular role permissions. Any authenticated user can currently write generic configuration or records. Production must add permission checks such as `configuration.manage` and `<module>.records.write`.

## Performance

Dynamic records use the tenant/module/update index and fixed `updated_at DESC` ordering. Pagination, filtering, search, caching, ETags, version preconditions and bulk operations are not implemented.
