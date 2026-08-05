# Authentication and session model

SuperCampus authenticates users with email and password only. Institution selection is server-side; the login request has no tenant or campus field.

## Login

    POST /api/auth/login
    Content-Type: application/json

    {
      "email": "admin@supercampus.local",
      "password": "your-password"
    }

The control database validates the password hash, requires an active user, active membership, and active institution, and selects the primary membership. The response returns the authenticated profile, effective roles, access token, expiry, and session identifier. HttpOnly access and refresh cookies are also issued.

## JWT and server session

The HS256 JWT contains:

- sub: user ID;
- tid: institution slug resolved from membership;
- sid: server-side session ID;
- roles: effective role keys;
- issuer, audience, issued-at, not-before, expiry, and token ID claims.

Every protected request validates the signature, issuer, audience, time claims, and matching non-revoked control-plane session. The middleware overwrites downstream identity headers from trusted claims. A caller-supplied x-tenant-id must match tid.

Access tokens are short-lived. POST /api/auth/refresh rotates the refresh token and issues a new access token. Reuse of an already rotated token revokes the session. POST /api/auth/logout revokes the session and clears cookies.

## Authorization and database routing

Effective roles and permissions are loaded dynamically from control-plane RBAC tables. After authorization, tenant-owned APIs resolve the signed tid through platform.tenant_databases and execute against the institution's database. Campus records do not participate in authentication or database routing.