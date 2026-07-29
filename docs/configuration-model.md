# Configuration model

Configuration is versioned, tenant-scoped metadata. Administrators edit drafts,
validate them against contracts, preview changes, and publish immutable versions.
The runtime resolves active configuration by tenant, campus, department, role,
and user context. Forms, views, navigation, dashboards, reports, rules, workflows,
and each module's policies consume this metadata. Publishing emits invalidation
events; no application rebuild is required.

Each domain module owns its configuration namespace and default templates while
the platform configuration service owns versioning, validation, publication,
rollback, inheritance, and audit.