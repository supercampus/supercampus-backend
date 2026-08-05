# Users module

**Status: Not Implemented as a dedicated HTTP API.**

No user CRUD, membership, password-management, or role-assignment router is mounted.

The generic route `/api/v1/{module_key}/records` may store arbitrary tenant JSON for registered modules, but it does not provide this module's validation, authorization, workflows, domain tables, events, reports, or integrations.
