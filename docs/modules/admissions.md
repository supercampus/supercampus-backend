# Admissions module

**Status: Not Implemented as a dedicated HTTP API.**

The package is scaffolded. Generic dynamic records are available, but admissions business APIs are not implemented.

The generic route `/api/v1/{module_key}/records` may store arbitrary tenant JSON for registered modules, but it does not provide this module's validation, authorization, workflows, domain tables, events, reports, or integrations.
