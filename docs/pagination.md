# Pagination, filtering, sorting and search

## Implemented pagination

Only CRM lead-list-derived endpoints implement pagination:

- `GET /api/v1/crm/leads`
- `GET /api/v1/crm/kanban/stages/{stage}/leads`
- `GET /api/v1/crm/kanban/stages/{stage}/count`
- Kanban/dashboard endpoints accept the same filters but internally force `limit=500`.

| Parameter | Type | Rule |
|---|---|---|
| `limit` | integer | Default 100; clamped to 1–500 |
| `offset` | integer | Default 0; negative values become 0 |

Responses are raw arrays inside `data`. Total-count and next-page metadata are **Not Implemented**. Cursor pagination is only used by the WebSocket event stream.

## CRM lead filters

`stage`, `substate`, `owner`, `source`, `globalStatus`, `priority`, `programId`, `search`, `createdFrom`, `createdTo`, `includeArchived`, `limit` and `offset` are supported.

- Counselor/frontline scope overrides `owner` with the authenticated user.
- Archived leads are excluded unless `includeArchived=true`.
- `programId` reads `interest->>'program_id'`.
- Dates are RFC 3339 timestamps.

## Search

`search` performs case-insensitive substring matching across full name, email and phone, or exact UUID text matching. The query uses the tenant/search index defined by the CRM migration, but leading-wildcard `ILIKE` can still become expensive at scale.

## Sorting

Lead order is fixed to `stage_entered_at ASC, created_at ASC`. Client-selected sort fields/directions are **Not Implemented**.

Generic dynamic-record lists sort by `updated_at DESC`. Pagination, filtering and search are **Not Implemented** for generic records.

## Caching

HTTP response caching and application-level query caching are **Not Implemented**.
