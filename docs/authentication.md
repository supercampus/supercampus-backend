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

## Password reset

Two public endpoints implement self-service recovery.

    POST /api/auth/forgot-password
    { "email": "admin@supercampus.local" }

Always answers `202` with the same body regardless of whether the address exists, so the endpoint cannot be used to enumerate accounts. When the address does belong to an active user, the API stores a reset token and emails a link to `APP_PUBLIC_URL/reset-password?token=...`. Requests are throttled to 3 per account per 15 minutes; throttled requests are silently ignored and still answer `202`.

    POST /api/auth/reset-password
    { "token": "...", "password": "at-least-12-characters" }

Consumes the token, writes a new pgcrypto hash, and revokes **every** active session for that account. Returns `400` when the token is unknown, expired, already used, or when the password is shorter than 12 characters.

Token handling mirrors refresh tokens: the raw value is only ever in the email, and `identity.password_reset_tokens` stores nothing but its SHA-256 digest. Tokens live for 60 minutes, are single-use, and any outstanding token for an account is invalidated once one is redeemed. Redemption uses `UPDATE ... RETURNING` inside a transaction, so concurrent submissions of the same link cannot both succeed.

Delivery is configured through the environment. With `SMTP_HOST` unset the API logs the reset link through `tracing` instead of sending it, which is the intended local-development behaviour. Setting `SMTP_HOST` switches to real SMTP delivery and makes `MAIL_FROM` required; see `.env.example`. A delivery failure is logged but never changes the API response, because a different status or latency would leak which addresses exist.

## JWT and server session

The HS256 JWT contains:

- sub: user ID;
- tid: institution slug resolved from membership;
- sid: server-side session ID;
- roles: effective role keys;
- issuer, audience, issued-at, not-before, expiry, and token ID claims.

Every protected request validates the signature, issuer, audience, time claims, and matching non-revoked control-plane session. The middleware overwrites downstream identity headers from trusted claims. A caller-supplied x-tenant-id must match tid.

Access tokens are short-lived. POST /api/auth/refresh rotates the refresh token and issues a new access token. Reuse of an already rotated token revokes the session. POST /api/auth/logout revokes the session and clears cookies.

## Server-driven navigation

    GET /api/v1/navigation

Returns the workspace and settings sections the caller may see. Sections live in
`platform.navigation_sections` per institution, each declaring the permissions that
reveal it, so a tenant administrator decides what a user can reach purely by composing
role grants. Visibility is recomputed from live grants on every request, so adding or
removing a permission changes the menu without a new token, a re-login, or a redeploy.

Resolution is ANY-of: a `*` grant, any explicitly listed permission, or any permission
prefixed with the section's `module_key`. `Settings` is emitted only when at least one
of its children is reachable. Institutions provisioned after migration 0019 have no
rows of their own and fall back to the platform defaults.

The client narrows the response to keys it has a view for, and clamps the open section
to the allowed set, so a deep link cannot open an ungranted area. The API remains the
enforcement boundary: hiding a section never substitutes for the permission checks on
the underlying routes.

## Realtime handshake token

    POST /api/auth/realtime-token

Mints a 60-second access token for the CRM WebSocket. Browsers cannot set headers on a
WebSocket handshake, and Next.js does not proxy upgrade requests, so the socket dials
this API directly from an origin that does not hold the session cookie. This endpoint
is called through the normal proxy, where the cookie does apply, and returns a token
the socket presents as `?access_token=`.

The query credential is honoured **only** on `/api/v1/crm/events`; every other route
continues to require a bearer header or cookie. The one-minute lifetime bounds the
exposure of a token that necessarily appears in a URL.

## Authorization and database routing

Effective roles and permissions are loaded dynamically from control-plane RBAC tables. After authorization, tenant-owned APIs resolve the signed tid through platform.tenant_databases and execute against the institution's database. Campus records do not participate in authentication or database routing.