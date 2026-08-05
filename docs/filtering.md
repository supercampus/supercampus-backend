# Filtering guide

Filtering is implemented only for CRM lead-derived reads.

Supported query parameters: `stage`, `substate`, `owner`, `source`, `globalStatus`, `priority`, `programId`, `createdFrom`, `createdTo`, `includeArchived`, `search`, `limit`, and `offset`.

All filters are combined with SQL `AND`. Non-read-all roles are always restricted to `assigned_to = authenticated_user`, overriding a requested owner. Dates are RFC 3339. Unknown query parameters are ignored by Serde.

Generic platform records, forms, templates, counselors and configuration do not expose filtering.
