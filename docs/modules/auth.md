# Authentication module endpoints

Base path: /api/auth. Detailed flows and security are in authentication.md.

| Method | Path | Auth | Request | Success | Storage/side effects |
|---|---|---|---|---|---|
| POST | /api/auth/login | No | LoginRequest | 200 LoginData plus cookies | Validate control-plane membership; insert auth session; sign JWT |
| POST | /api/auth/refresh | sc_session | Cookie only | 200 LoginData plus rotated cookies | Lock and rotate session hash; reuse revokes |
| GET | /api/auth/me | Bearer or sc_access | None | 200 SessionData | Read active control-plane session and institution |
| POST | /api/auth/logout | Optional credentials | None | 204 | Revoke matching session; clear cookies |

## LoginRequest

| Field | Required | Validation |
|---|---|---|
| email string | Yes | Normalized and matched to an active identity |
| password string | Yes | Verified against the stored password hash |

Login does not accept tenantId or campus. The primary active institution membership determines the JWT tid claim and tenant database route.

LoginData contains student, accessToken, tokenType, expiresAt, sessionId, and roles.