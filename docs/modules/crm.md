# CRM API endpoints

Base path: `/api/v1/crm`. Protected operations require a valid access JWT/session. Middleware injects canonical tenant, user and role headers; clients must not choose identity headers. Expanded example payloads are in [CRM_API_ENDPOINTS.md](../CRM_API_ENDPOINTS.md).

## System and permissions

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/health` | Public | None | 200 module status | None |
| GET | `/roles` | Authenticated | None | 200 role catalog | None |
| GET | `/permissions/effective` | Authenticated | None | 200 effective flags/scope | None |

## Leads

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/leads` | `crm.leads.read`; CRM-accessible role; counselor scoped | `LeadFilters` query | 200 lead array | Read `crm.leads`; permission audit |
| POST | `/leads` | Frontline/counselor/manager | `CreateLeadRequest` | 201 lead | Transaction: duplicate lookup, lead insert, optional auto-assignment, history/outbox |
| GET | `/leads/{id}` | Read-all or assigned owner | UUID | 200 lead | Read lead; permission audit |
| PATCH | `/leads/{id}` | Manager or assigned frontline/counselor | `UpdateLeadRequest` | 200 lead | Update lead; outbox |
| DELETE | `/leads/{id}` | Manager | UUID | 204 | Soft delete `deleted_at` |
| POST | `/leads/{id}/assign` | Manager; counselor may claim unassigned self | `AssignLeadRequest` | 200 lead | Transaction: lock lead, assignment/history/outbox |
| POST | `/leads/{id}/reassign` | Manager or valid claim policy; reason required | `AssignLeadRequest` | 200 lead | Same tables; reassignment history |
| GET | `/leads/{id}/timeline` | Read-all or assigned owner | UUID | 200 stage/communication history | Read `stage_history` and `communications` |

## Pipeline actions and aliases

Each write returns 200 lead, audits permission, updates `crm.leads` in a transaction, writes relevant history and emits `crm.outbox_events`.

| Method | Canonical path | Compatibility alias | Permission | Body |
|---|---|---|---|---|
| POST | `/leads/{id}/stage/move` | None | Owner-update policy plus role target limit and tenant workflow toggle | `MoveStageRequest` |
| POST | `/leads/{id}/stage/prospect` | `/leads/{id}/prospect` | Assigned counselor/manager with hold capability | `IntakeStatusRequest` |
| POST | `/leads/{id}/stage/defer` | `/leads/{id}/defer` | Assigned counselor/manager with hold capability | `IntakeStatusRequest` |
| POST | `/leads/{id}/stage/hold` | `/leads/{id}/hold` | Assigned counselor/manager | `HoldRequest` |
| POST | `/leads/{id}/stage/release-hold` | `/leads/{id}/release-hold` | Assigned counselor/manager | `ReasonRequest` |
| POST | `/leads/{id}/stage/archive` | `/leads/{id}/archive` | Manager | `ArchiveRequest` |
| POST | `/leads/{id}/stage/unarchive` | `/leads/{id}/unarchive` | Manager | `UnarchiveRequest` |

Aliases are separate mounted operations and have identical behavior/statuses.

## Kanban, dashboard and events

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/kanban/board` | `crm.leads.read` | Lead filters | 200 nine-stage board | Reads up to 500 leads |
| GET | `/kanban/my-board` | Same; role scope still applied | Lead filters | 200 board | Reads up to 500 |
| GET | `/kanban/stages` | Authenticated | None | 200 static stage catalog | None |
| GET | `/kanban/stages/{stage}/leads` | `crm.leads.read` | stage + filters | 200 array | Read leads |
| GET | `/kanban/stages/{stage}/count` | `crm.leads.read` | stage + filters | 200 stage/count | Reads matching page; count is page length |
| GET | `/dashboard` | `crm.leads.read` | Lead filters | 200 board payload | Reads up to 500 |
| GET | `/dashboard/operations` | `crm.dashboard.read` | None | 200 command-center payload | Aggregates scoped leads, tenant campaigns, automations, counselor SLA, quality and case data |
| GET | `/events?cursor=N` | `crm.dashboard.read` | WebSocket upgrade | 101 | Polls tenant `outbox_events` each second |

The stage-count endpoint does not execute `COUNT(*)` and can be capped by pagination; clients must not interpret it as an unlimited global count.

## Dynamic forms

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/forms` | CRM-accessible role | None | 200 forms | Read `crm.forms` |
| POST | `/forms` | Admissions manager/program advisor | `CreateFormRequest` | 201 form v1 draft | Insert form |
| GET | `/forms/{id}` | CRM-accessible role | UUID | 200 form | Read form |
| PUT | `/forms/{id}` | Form manager | `UpdateFormRequest` | 200 new version | Update form/schema/version |
| PATCH | `/forms/{id}` | Form manager | `UpdateFormRequest` | 200 new version | Identical to PUT |
| DELETE | `/forms/{id}` | Form manager | UUID | 204 | Soft delete form |
| POST | `/forms/{id}/publish` | Form manager | None | 200 form | Set published |
| POST | `/forms/{id}/unpublish` | Form manager | None | 200 form | Set draft |
| POST | `/forms/{id}/submit` | Published form or form manager; internal forms require staff | `SubmitFormRequest` | 201 submission | Insert submission; enquiry may create lead |
| POST | `/public/forms/{id}/submit` | Public; `x-tenant-id` required | `SubmitFormRequest` | 201 submission | Same; generated public actor |
| GET | `/forms/{id}/submissions` | Staff excluding frontline | UUID | 200 submissions | Read form submissions |

If enquiry lead creation succeeds but submission insert fails, the code attempts a compensating soft delete; this is not one atomic transaction.

## Communications and templates

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| POST | `/communications/whatsapp` | Assigned communicator | `SendCommunicationRequest` | 200 communication | Insert communications + outbox; no provider delivery worker |
| POST | `/communications/email` | Assigned communicator | Same | 200 communication | Same |
| POST | `/communications/calls` | Assigned communicator | Same; outcome required | 200 communication | Same |
| GET | `/templates` | CRM-accessible role | None | 200 templates | Read templates |
| POST | `/templates` | Manager | `CreateTemplateRequest` | 201 template | Upsert template |
| GET | `/communications/templates` | Same as `/templates` | None | 200 templates | Alias |
| POST | `/communications/templates` | Same as `/templates` | Same | 201 template | Alias |

## Assignment and dynamic configuration

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/assignment/counselors` | CRM-accessible role | None | 200 workload/capacity | Read capacity + lead counts |
| PUT | `/assignment/counselors` | Manager/assign role | `CounselorCapacityRequest` | 200 counselor | Upsert capacity |
| GET | `/configuration` | CRM-accessible role | None | 200 toggles | Read workflow/automation toggles |
| PUT | `/configuration/workflow-toggles` | Manager | `WorkflowToggleRequest` | 200 toggle | Upsert workflow policy |
| PUT | `/configuration/automation-toggles` | Manager | `AutomationToggleRequest` | 200 toggle | Upsert automation policy |

## Campaign performance

| Method | Path | Permission | Request | Success | Storage/effects |
|---|---|---|---|---|---|
| GET | `/campaigns` | `crm.reports.read`; read-all role | None | 200 campaign array | Reads tenant-isolated `crm.campaigns` |
| POST | `/campaigns` | `crm.configuration.manage`; manager | `CreateCampaignRequest` | 201 campaign | Upserts by tenant/name and supplies dashboard budget, CPL and ROI calculations |

Campaign amounts and landing-page counts must be non-negative. Status is
`draft`, `active`, `paused`, or `completed`; an end date cannot precede the
start date.

## Query schema: LeadFilters

`stage`, `substate`, `owner`, `source`, `globalStatus`, `priority`, `programId`, `search`, `createdFrom`, `createdTo`, `includeArchived`, `limit` and `offset`. See [pagination.md](../pagination.md).

## Body validation

- Create lead: nonblank student name; at least phone or email; `priority` is `low|medium|high|urgent`. `source` is structurally required but blank is not rejected.
- Update lead: optional fields; provided priority uses the same enum.
- Reassignment: nonblank `reason`.
- Stage: known primary stage and valid substate transition; archived is terminal except unarchive; role target ceiling and tenant workflow toggle apply.
- Prospect: lead must previously reach Qualified. Intake year and program ID are structurally required; range/nonblank validation is not implemented.
- Hold/release: reason is structurally required but blank is not rejected.
- Archive reason must exactly match: Academic Ineligibility, Age Criteria Not Met, Calls Not Answered, Duplicate Lead, Education Gap, Education Loan Rejected, Fake Documents, Financial Ineligibility, Full Scholarship Required, Health Issues, Insufficient Documents, Intake Deadline Passed, Interview No Show, Invalid Number, Lost to Competitor, Low Score, No Offer, No Offer from Preferred Choice, No Revenue Potential, Not Happy with Service, Not Interested in Engineering, Not Reachable, Not Satisfied with Offering, Offer Expired, Others, Program Full/Closed, Program Not Available, Program Not Offered, Refund Initiated, Spam, Student Opted Out.
- Unarchive: restore stage cannot be Archived; substate must belong to restore stage; reason structurally required.
- Create form: name and form type nonblank. JSON schema correctness is not validated.
- Update form: `schema` required; optional name.
- Enquiry form submission: form published unless manager; nonblank name and phone or email.
- Communication: channel fixed by endpoint; WhatsApp requires previously Qualified; call requires nonblank outcome.
- Counselor defaults: active true, max capacity 100, average response 60 minutes, conversion 0. Range checks are not implemented.
- Workflow/automation stages must parse as known stages. Trigger/action/role JSON shapes are not validated.

Unknown fields, string length limits, email format, phone format, payload-size limits and idempotency keys are not implemented.

## Statuses and errors

Common statuses: `200`, `201`, `204`, `400`, `401`, `403`, `404`, `409`, `500` and `503`. `422` and `429` are not emitted. See [errors.md](../errors.md).

All permission-gated service calls insert `crm.permission_audit` with decision and reason before returning. External notifications and background provider jobs are not implemented; “communication queued” means persisted plus outbox event.
