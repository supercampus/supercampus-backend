# Errors and status codes

## Platform error envelope

```json
{
  "error": "Authentication is required",
  "code": "unauthorized"
}
```

Codes implemented by `ApiError`: `bad_request`, `unauthorized`, `forbidden`, `not_found` and `internal_error`.

## CRM error envelope

```json
{
  "error": {
    "code": "validation_error",
    "message": "CRM validation failed: priority must be low, medium, high, or urgent"
  }
}
```

CRM codes: `not_found`, `unauthorized`, `forbidden`, `validation_error`, `conflict`, `database_unavailable` and `storage_error`.

## Status reference

| Status | Implemented meaning |
|---:|---|
| 200 | Successful read/update/login/refresh |
| 201 | Lead, form, form submission or template created |
| 204 | Record/form/lead deletion or logout |
| 400 | JSON extraction error, platform validation, or CRM validation |
| 401 | Missing/invalid/expired access or refresh credential |
| 403 | Tenant mismatch, CRM role/ownership/configuration denial |
| 404 | Unknown platform resource or missing CRM entity |
| 409 | CRM duplicate/conflicting state |
| 422 | **Not Implemented**; semantic validation currently returns 400 |
| 429 | **Not Implemented**; rate limiting is absent |
| 500 | Unexpected platform/storage failure |
| 503 | CRM database not configured |
| 101 | Successful CRM WebSocket upgrade |

Axum may emit a framework-generated plain-text `400` or `415` when JSON syntax or content type is invalid; this is not normalized to the application error envelope.

## Retry guidance

- Retry `500` and `503` only with bounded exponential backoff.
- Do not automatically retry `400`, `401`, `403`, `404` or `409`.
- After `401`, attempt one refresh. If refresh fails, require login.
- A `409` requires the client to reload current state and resolve the conflict.
