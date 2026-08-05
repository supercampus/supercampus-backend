# Search guide

CRM lead search is available through `GET /api/v1/crm/leads?search=<value>` and lead-derived stage/board endpoints.

It matches:

- Full name using case-insensitive substring search.
- Email using case-insensitive substring search.
- Phone using case-insensitive substring search.
- Lead UUID using exact text equality.

Search is tenant- and role-scoped and excludes soft-deleted records. Archived records remain excluded unless `includeArchived=true`.

Fuzzy ranking, full-text search, highlighting, normalization, cursor pagination and search analytics are **Not Implemented**.
