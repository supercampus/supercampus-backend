# Rate limits

Rate limiting is **Not Implemented**. The backend does not emit `429 Too Many Requests` or rate-limit headers.

Before production, apply limits at the ingress/API gateway and add application limits for login, refresh, public form submission, search and communication endpoints. Recommended policies are operational guidance, not current behavior:

- Login: 5 attempts per account/IP per 15 minutes.
- Refresh: 30 requests per session per minute.
- Public enquiry: 10 submissions per IP per hour plus bot protection.
- Authenticated reads: tenant/user quota.
- Communications: channel- and provider-specific quotas.

A future `429` response should use the documented error envelope and include `Retry-After`.
