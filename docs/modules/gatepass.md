# Gatepass module

**Status: First configurable workflow slice implemented through platform workflow endpoints.**

The package is scaffolded; request, approval, QR, and movement APIs are not implemented.

The generic route `/api/v1/{module_key}/records` may store arbitrary tenant JSON for registered modules, but it does not provide this module's validation, authorization, workflows, domain tables, events, reports, or integrations.

## Configurable outpass workflow

The first MVP slice exposes tenant-specific Gatepass Outpass workflow definitions
through:

- `GET /api/v1/workflows/gatepass/outpass`
- `POST /api/v1/workflows/gatepass/outpass/transitions/validate`

Workflow definitions are versioned tenant configuration. The platform reads
`configuration.runtime_documents` namespace `workflows.gatepass.outpass` when a
tenant has a saved definition, and falls back to seed definitions for local proof
tenants:

- `tenant-a` / `college-1`: student submit -> parent approve -> warden approve -> security verify -> complete.
- `tenant-b` / `college-2`: student submit -> warden approve -> security verify -> complete.

The transition validator is the backend authority for valid next steps. Clients
send the current state and requested action; the backend resolves the tenant
workflow and checks the transition's required permission before returning the next
state.
