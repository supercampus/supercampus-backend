# CRM backend implementation map

The source requirement documents in this directory are authoritative. The Rust implementation lives in `modules/crm` and is mounted by `apps/platform-api` at `/api/v1/crm`.

## Runtime boundary

Production authentication is expected to validate a bearer token at the API gateway and inject trusted `x-tenant-id`, `x-user-id`, and `x-user-role` headers. The CRM service independently enforces role, ownership, transition, and tenant policies and writes every permission decision to `crm.permission_audit`. Direct client-supplied identity headers must not be trusted at a public production ingress.

The public enquiry route is `/api/v1/crm/public/forms/{id}/submit`. It needs `x-tenant-id` from the institution-specific public hostname/gateway, requires no user account, records the versioned form response, and creates the lead.

## Requirement-to-code map

| Requirement module | Rust implementation | PostgreSQL tables |
|---|---|---|
| Lead capture and duplicate detection | `application/services.rs`, `infrastructure/postgres/mod.rs` | `crm.leads`, `crm.stage_history` |
| Weighted counselor assignment | Postgres repository score and row lock | `crm.counselor_capacity`, `crm.assignment_history` |
| Nine-stage pipeline | `domain/pipeline.rs` | `crm.leads`, `crm.stage_history`, `crm.workflow_toggles` |
| Role and ownership access | `domain/permissions.rs`, service authorization | `crm.permission_audit` |
| Dynamic form builder | form endpoints and repository | `crm.forms`, `crm.form_submissions` |
| WhatsApp, email, calls, templates | communication endpoints and outbox | `crm.communications`, `crm.communication_templates`, `crm.outbox_events` |
| Archive and hold | pipeline actions | `crm.archive_records`, `crm.holds` |
| ERP handoff | Offer Accepted validation and outbox event | `crm.outbox_events`, ERP fields on `crm.leads` |
| Unified dashboard | `/kanban/*` and `/dashboard` | filtered reads over `crm.leads` |
| Tenant configuration | `/configuration/*` | `crm.workflow_toggles`, `crm.automation_toggles` |

## Local verification

Start from `supercampus-backend` with `cargo run -p supercampus-platform-api`. The API listens on port 4000 by default. Health: `GET http://localhost:4000/api/v1/crm/health`.

All database mutations use a tenant-scoped transaction and set `app.tenant_id`; row-level security is enabled for every CRM table. External messages and ERP requests are written to the transactional outbox so provider workers can retry without losing the originating transaction. The `/api/v1/crm/events?cursor=0` WebSocket streams those committed events in tenant sequence order; reconnect with the last received cursor to resume.