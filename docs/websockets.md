# CRM WebSocket events

## Connection

```text
ws://localhost:4000/api/v1/crm/events?cursor=0
```

The handshake requires an authenticated session. Browser clients should rely on the same-origin HTTP-only `sc_access` cookie; non-browser clients may send a bearer header.

`cursor` is an optional non-negative outbox sequence. The server replays later events, polls PostgreSQL once per second, and advances the cursor from each message.

Example event:

```json
{
  "cursor": 42,
  "eventType": "crm.lead.updated",
  "aggregateId": "2f3e0a2f-6a06-4c65-a220-43a3737573d0",
  "payload": {},
  "createdAt": "2026-07-31T12:00:00Z"
}
```

Security and isolation:

- JWT/session middleware supplies the tenant.
- Queries filter `crm.outbox_events.tenant_id` and run with CRM RLS context.
- `crm.dashboard.read` and a CRM-accessible role are required.
- There is no separate WebSocket rate limit or connection quota.

On repository failure, the server sends `crm.stream_error` and closes. Ping/pong policy, client acknowledgements, retention guarantees and event-schema registry are **Not Implemented**.
