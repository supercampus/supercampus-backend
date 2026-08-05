# SuperCampus CRM API Endpoints

## Connection

- Backend base URL: `http://localhost:4000`
- CRM base URL: `http://localhost:4000/api/v1/crm`
- WebSocket URL: `ws://localhost:4000/api/v1/crm/events?cursor=0`
- OpenAPI contract: `modules/crm/openapi/crm.yaml`

The backend serves JSON APIs. The frontend user interface runs separately on
`http://localhost:3000`.

## Authentication context

Protected CRM endpoints require a signed access token:

```http
Authorization: Bearer <access-token>
Content-Type: application/json
```

The platform authorization middleware verifies the JWT and server-side session,
reloads tenant roles and grants from PostgreSQL, then injects canonical tenant,
user, role, permission, and permission-scope headers. Client-supplied identity
headers are overwritten.

`GET /health` and `POST /public/forms/{id}/submit` remain public. Public form
submission requires `x-tenant-id` so the enquiry can be routed to a tenant.

## System and access

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `GET` | `/health` | Public | CRM service health |
| `GET` | `/roles` | `authorization.roles.read` | Tenant-created roles and grants |
| `GET` | `/permissions/effective` | Authenticated | Effective permissions and record scope |

## Leads

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `POST` | `/leads` | Front office, counselor, manager | Create a lead |
| `POST` | `/leads/import` | `crm.leads.import` | Import up to 1000 validated CSV rows |
| `GET` | `/leads` | CRM users, role scoped | List and filter leads |
| `GET` | `/leads/{id}` | Owner or read-all role | Get lead details |
| `PATCH` | `/leads/{id}` | Owner or manager | Update lead fields |
| `DELETE` | `/leads/{id}` | Manager | Soft-delete a lead |
| `POST` | `/leads/{id}/assign` | Manager, or counselor claiming an unassigned lead | Assign a lead |
| `POST` | `/leads/{id}/reassign` | Manager | Reassign a lead; reason required |
| `GET` | `/leads/{id}/timeline` | Owner or read-all role | Get stage and communication history |

### Bulk lead import

The web application parses the CSV locally, lets the tenant user map headers,
previews invalid and duplicate rows, then sends normalized JSON to:

```http
POST /api/v1/crm/leads/import
Authorization: Bearer <access-token>
Content-Type: application/json
```

```json
{
  "duplicateStrategy": "skip",
  "rows": [
    {
      "rowNumber": 2,
      "source": "Education fair",
      "student": {
        "name": "Asha Kumar",
        "email": "asha@example.com",
        "phone": "9876543210"
      },
      "interest": { "programName": "B.Tech CSE" },
      "priority": "high"
    }
  ]
}
```

`duplicateStrategy` is `skip` (safe default) or `flag`. Duplicate detection
uses phone or email within the tenant and within the same import. The response
contains `total`, `created`, `skipped`, `failed`, and a result for every
submitted CSV row. Invalid rows do not prevent valid rows from importing.

### Lead list query parameters

`GET /leads` supports:

| Parameter | Description |
|---|---|
| `stage` | Primary pipeline stage |
| `substate` | Stage substate |
| `owner` | Assigned user ID |
| `source` | Lead source |
| `globalStatus` | `prospect`, `deferred`, `on_hold`, or `archive` |
| `priority` | `low`, `medium`, `high`, or `urgent` |
| `programId` | Program identifier inside lead interest |
| `search` | Name, email, phone, or lead UUID |
| `createdFrom` | ISO-8601 start timestamp |
| `createdTo` | ISO-8601 end timestamp |
| `includeArchived` | Include archived leads |
| `limit` | Page size |
| `offset` | Page offset |

## Pipeline actions

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `POST` | `/leads/{id}/stage/move` | Authorized owner/manager | Validate and perform a stage transition |
| `POST` | `/leads/{id}/stage/prospect` | Counselor or manager | Mark as Prospect; lead must have reached Qualified |
| `POST` | `/leads/{id}/stage/defer` | Counselor or manager | Defer to a selected intake |
| `POST` | `/leads/{id}/stage/hold` | Counselor or manager | Freeze progression while preserving current stage |
| `POST` | `/leads/{id}/stage/release-hold` | Counselor or manager | Release an active hold |
| `POST` | `/leads/{id}/stage/archive` | Manager | Archive using one of the approved reasons |
| `POST` | `/leads/{id}/stage/unarchive` | Manager | Restore an archived lead |

The following short aliases call the same handlers:

```text
POST /leads/{id}/prospect
POST /leads/{id}/defer
POST /leads/{id}/hold
POST /leads/{id}/release-hold
POST /leads/{id}/archive
POST /leads/{id}/unarchive
```

## Kanban, dashboard, and real-time events

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `GET` | `/kanban/board` | CRM users, role scoped | Full nine-stage Kanban board |
| `GET` | `/kanban/my-board` | CRM users | Personalized assigned-lead board |
| `GET` | `/kanban/stages` | Public metadata | Stages and substates |
| `GET` | `/kanban/stages/{stage}/leads` | CRM users, role scoped | Leads in one stage |
| `GET` | `/kanban/stages/{stage}/count` | CRM users, role scoped | Lead count for one stage |
| `GET` | `/dashboard` | CRM users, role scoped | Filtered dashboard payload |
| `GET` | `/dashboard/operations` | CRM users, role scoped | CRM command-center metrics and operational queues |
| `GET` WebSocket upgrade | `/events?cursor={cursor}` | CRM users | Ordered tenant event stream |

WebSocket clients should store the last received `cursor` and send it when
reconnecting.

## Forms

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `POST` | `/forms` | Admissions manager or program advisor | Create a draft form |
| `GET` | `/forms` | CRM users | List forms |
| `GET` | `/forms/published/lead-capture` | Form readers or lead creators | Latest published lead-capture schema |
| `GET` | `/forms/{id}` | CRM users | Get a form definition |
| `PUT` | `/forms/{id}` | Form manager | Update name, form type, and schema; increment version and return to draft |
| `PATCH` | `/forms/{id}` | Form manager | Same update operation as `PUT` |
| `DELETE` | `/forms/{id}` | Form manager | Soft-delete a form |
| `POST` | `/forms/{id}/publish` | Form manager | Publish a form |
| `POST` | `/forms/{id}/unpublish` | Form manager | Return a form to draft |
| `POST` | `/forms/{id}/submit` | Admission staff | Record a versioned internal submission |
| `GET` | `/forms/{id}/submissions` | Counselor, manager, or approved read role | List submissions |
| `POST` | `/public/forms/{id}/submit` | Public; tenant header required | Submit an enquiry form and create a CRM lead |

The public form endpoint only requires:

```http
x-tenant-id: tenant-local
Content-Type: application/json
```

## Communications

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `POST` | `/communications/whatsapp` | Assigned counselor or manager | Queue WhatsApp communication after qualification |
| `POST` | `/communications/email` | Assigned counselor or manager | Queue email communication |
| `POST` | `/communications/calls` | Assigned counselor or manager | Record a call and required outcome |
| `GET` | `/communications/templates` | CRM users | List tenant communication templates |
| `POST` | `/communications/templates` | Manager | Create or update a template |

Template aliases:

```text
GET  /templates
POST /templates
```

## Assignment configuration

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `GET` | `/assignment/counselors` | CRM users | List counselor workload and capacity |
| `PUT` | `/assignment/counselors` | Manager | Add or update counselor assignment configuration |

Digital leads are assigned using workload, response time, and conversion-rate
weights. Offline leads remain available for manual assignment.

## Dynamic tenant configuration

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `GET` | `/configuration` | CRM users | Read workflow and automation toggles |
| `PUT` | `/configuration/workflow-toggles` | Manager | Enable, disable, or role-restrict a transition |
| `PUT` | `/configuration/automation-toggles` | Manager | Configure runtime stage automations |

Configuration changes take effect on subsequent requests without rebuilding or
redeploying the application.

## Campaign performance

| Method | Endpoint | Access | Description |
|---|---|---|---|
| `GET` | `/campaigns` | Read-all CRM/report role | List persisted tenant campaign finance data |
| `POST` | `/campaigns` | CRM configuration manager | Create or update a campaign by tenant and name |

Campaign data is the source for budget-used, landing-page, active-UTM, CPL and
ROI values in `/dashboard/operations`. If a source has leads but no campaign
spend, `costPerLead` and `roi` are returned as `null`; the API never invents a
cost or return value.

## Expected success returns

Except for health, WebSocket, and delete operations, successful HTTP responses
are wrapped in a top-level `data` property.

### Endpoint return matrix

| Endpoint | Status | Expected response |
|---|---:|---|
| `GET /health` | `200` | Direct `Health` object |
| `GET /roles` | `200` | `data: Role[]` |
| `GET /permissions/effective` | `200` | `data: EffectivePermissions` |
| `POST /leads` | `201` | `data: Lead` |
| `GET /leads` | `200` | `data: Lead[]` |
| `GET /leads/{id}` | `200` | `data: Lead` |
| `PATCH /leads/{id}` | `200` | `data: Lead` |
| `DELETE /leads/{id}` | `204` | Empty response body |
| `POST /leads/{id}/assign` | `200` | `data: Lead` with updated assignment |
| `POST /leads/{id}/reassign` | `200` | `data: Lead` with updated assignment |
| `POST /leads/import` | `200` | `data: BulkImportLeadsResponse` with per-row results |
| `GET /leads/{id}/timeline` | `200` | `data: LeadTimeline` |
| All successful pipeline action endpoints | `200` | `data: Lead` with updated stage/global status |
| `GET /kanban/board` | `200` | `data: KanbanBoard` |
| `GET /kanban/my-board` | `200` | `data: KanbanBoard` |
| `GET /kanban/stages` | `200` | `data: StageDefinition[]` |
| `GET /kanban/stages/{stage}/leads` | `200` | `data: Lead[]` |
| `GET /kanban/stages/{stage}/count` | `200` | `data: StageCount` |
| `GET /dashboard` | `200` | `data: KanbanBoard` |
| `GET /dashboard/operations` | `200` | `data: CrmOperationsDashboard` |
| `GET /campaigns` | `200` | `data: Campaign[]` |
| `POST /campaigns` | `201` | `data: Campaign` |
| `GET /events?cursor={cursor}` | `101` | WebSocket upgrade followed by `RealtimeEvent` messages |
| `POST /forms` | `201` | `data: FormDefinition` |
| `GET /forms` | `200` | `data: FormDefinition[]` |
| `GET /forms/{id}` | `200` | `data: FormDefinition` |
| `PUT/PATCH /forms/{id}` | `200` | `data: FormDefinition` with incremented version |
| `DELETE /forms/{id}` | `204` | Empty response body |
| `POST /forms/{id}/publish` | `200` | `data: FormDefinition` with `status: "published"` |
| `POST /forms/{id}/unpublish` | `200` | `data: FormDefinition` with `status: "draft"` |
| `POST /forms/{id}/submit` | `201` | `data: FormSubmission` |
| `POST /public/forms/{id}/submit` | `201` | `data: FormSubmission` including `createdLeadId` |
| `GET /forms/{id}/submissions` | `200` | `data: FormSubmission[]` |
| Communication send/log endpoints | `200` | `data: Communication` |
| `GET /communications/templates` | `200` | `data: CommunicationTemplate[]` |
| `POST /communications/templates` | `201` | `data: CommunicationTemplate` |
| `GET /assignment/counselors` | `200` | `data: CounselorCapacity[]` |
| `PUT /assignment/counselors` | `200` | `data: CounselorCapacity` |
| `GET /configuration` | `200` | `data: CrmConfiguration` |
| `PUT /configuration/workflow-toggles` | `200` | `data: WorkflowToggle` |
| `PUT /configuration/automation-toggles` | `200` | `data: AutomationToggle` |

### Health

`GET /health`

```json
{
  "module": "crm",
  "status": "ok",
  "contract": "v1"
}
```

### Roles

`GET /roles`

```json
{
  "data": [
    {
      "key": "admission_counselor",
      "tier": "counselor"
    },
    {
      "key": "admissions_manager",
      "tier": "manager"
    },
    {
      "key": "marketing_executive",
      "tier": "marketing"
    }
  ]
}
```

The actual response contains the complete configured CRM role catalog.

### Effective permissions

`GET /permissions/effective`

```json
{
  "data": {
    "userId": "admin-001",
    "role": {
      "key": "admissions_manager",
      "tier": "manager"
    },
    "scope": "all",
    "permissions": {
      "access": true,
      "createLead": true,
      "assign": true,
      "archive": true,
      "hold": true,
      "manageForms": true,
      "communicate": true,
      "manageConfiguration": true,
      "triggerErp": true
    }
  }
}
```

Users without read-all permission receive `"scope": "assigned"`.

### Lead

Lead creation, retrieval, update, assignment, reassignment, and pipeline actions
return the current complete lead:

```json
{
  "data": {
    "id": "07fb390f-d35d-4570-bd26-8a0573532b03",
    "tenantId": "tenant-local",
    "fullName": "Test Student",
    "email": "student@example.com",
    "phone": "9000000001",
    "whatsapp": "9000000001",
    "parentName": null,
    "parentPhone": null,
    "source": "Google Ads",
    "sourceDetail": {
      "campaign": "July Admissions"
    },
    "academic": {},
    "interest": {
      "program_id": "btech-cse"
    },
    "pipelineKey": "pre-admission",
    "stageKey": "enquiry",
    "substateKey": "new",
    "globalStatus": null,
    "globalStatusData": {},
    "assignedTo": "counselor-001",
    "assignedBy": "admin-001",
    "assignmentType": "auto",
    "priority": "high",
    "followUpAt": null,
    "preferredChannel": "whatsapp",
    "consentGiven": true,
    "feePaymentConfirmed": false,
    "documentsVerified": false,
    "scholarshipStatus": "none",
    "erpStatus": "not_ready",
    "erpStudentId": null,
    "erpEnrollmentNumber": null,
    "duplicateOf": null,
    "customFields": {},
    "createdBy": "admin-001",
    "stageEnteredAt": "2026-07-30T00:00:00Z",
    "createdAt": "2026-07-30T00:00:00Z",
    "updatedAt": "2026-07-30T00:00:00Z"
  }
}
```

`GET /leads` and stage lead lists return an array of complete lead objects:

```json
{
  "data": [
    {
      "id": "07fb390f-d35d-4570-bd26-8a0573532b03",
      "tenantId": "tenant-local",
      "fullName": "Test Student",
      "stageKey": "enquiry",
      "substateKey": "new",
      "assignedTo": "counselor-001",
      "priority": "high"
    }
  ]
}
```

The abbreviated entry above represents the array shape; actual entries contain
all fields from the complete `Lead` response.

### Lead timeline

`GET /leads/{id}/timeline`

```json
{
  "data": {
    "stageHistory": [
      {
        "id": "7dbcf6e7-23fa-43d7-9bf8-b84fd50d2ab8",
        "fromStage": "enquiry",
        "fromSubstate": "new",
        "toStage": "contact_attempted",
        "toSubstate": "contacted",
        "actorId": "admin-001",
        "actorRole": "admissions_manager",
        "reason": "Initial outreach completed",
        "notes": null,
        "createdAt": "2026-07-30T00:05:00Z"
      }
    ],
    "communications": [
      {
        "id": "80f90853-d7f3-47ca-9c71-ad360d40cb2e",
        "leadId": "07fb390f-d35d-4570-bd26-8a0573532b03",
        "channel": "email",
        "direction": "outbound",
        "templateKey": "course_information",
        "subject": "Program information",
        "content": {
          "message": "Requested program information"
        },
        "outcome": null,
        "status": "queued",
        "actorId": "admin-001",
        "createdAt": "2026-07-30T00:06:00Z"
      }
    ]
  }
}
```

### CRM operations dashboard

`GET /dashboard/operations`

```json
{
  "data": {
    "scope": "all",
    "headline": {
      "leadIntake": 7,
      "followUpsDue": 2,
      "campaignRoi": 2.4,
      "counselorSla": 71
    },
    "operations": {
      "newLeads": 3,
      "contactDue": 2,
      "qualified": 1,
      "applications": 1,
      "accepted": 0,
      "priorityQueue": [
        {
          "leadId": "07fb390f-d35d-4570-bd26-8a0573532b03",
          "fullName": "Test Student",
          "course": "btech-cse",
          "city": "Chennai",
          "source": "Google Ads",
          "assignedTo": "Admission Counselor",
          "priority": "high",
          "followUpAt": "2026-08-01T08:00:00Z"
        }
      ]
    },
    "automations": [
      {
        "id": "ef45bf3f-7773-4f4c-bc79-c2a63009456d",
        "label": "Send Whatsapp",
        "stage": "qualified",
        "triggerName": "on_enter",
        "action": "send_whatsapp",
        "templateKey": "qualified_confirmation",
        "enabled": true
      }
    ],
    "sourceRoi": [
      {
        "source": "Google Ads",
        "leads": 4,
        "applications": 2,
        "budget": 50000.0,
        "spent": 20000.0,
        "attributedRevenue": 48000.0,
        "costPerLead": 5000.0,
        "roi": 2.4
      }
    ],
    "campaignSummary": {
      "budgetUsedPercent": 40,
      "landingPages": 2,
      "activeUtm": 1
    },
    "health": {
      "score": 71,
      "duplicateDetection": 100,
      "sourceAttribution": 100,
      "postQualifiedWhatsapp": 50
    },
    "cases": {
      "open": 2,
      "counts": {
        "prospect": 0,
        "deferred": 1,
        "on_hold": 1,
        "archive": 0
      },
      "items": []
    }
  }
}
```

All values are computed from tenant-scoped PostgreSQL data. Counselor-scoped
roles receive only their assigned leads. `campaignRoi` and `sourceRoi[].roi`
are revenue/spend multiples. `counselorSla` is the percentage of scheduled
follow-ups that are not overdue. `health.score` is the percentage of active
lead records containing owner, follow-up, and source. Duplicate detection is
the percentage without an unresolved `duplicateOf` marker. Post-qualified
WhatsApp is the percentage of qualified-or-later leads with a stored WhatsApp
communication.

### Campaign

`POST /campaigns`

```json
{
  "name": "August Search",
  "source": "Google Ads",
  "budget": 50000,
  "spent": 20000,
  "attributedRevenue": 48000,
  "landingPages": 2,
  "utmCode": "august-search",
  "status": "active",
  "startsOn": "2026-08-01",
  "endsOn": "2026-08-31"
}
```

The success response wraps the persisted campaign in `data`. Posting the same
tenant/name updates its financial and attribution values. Campaigns are stored
in `crm.campaigns` with tenant RLS.

### Kanban board and dashboard

`GET /kanban/board`, `GET /kanban/my-board`, and `GET /dashboard`

```json
{
  "data": {
    "pipeline": {
      "key": "pre-admission",
      "name": "Pre-Admission Pipeline"
    },
    "scope": "all",
    "stages": [
      {
        "key": "enquiry",
        "order": 1,
        "substates": [
          "new",
          "contact_attempted",
          "contacted",
          "nurture",
          "qualified",
          "converted"
        ],
        "count": 1,
        "leads": []
      }
    ],
    "total": 1
  }
}
```

Each item inside `stages[].leads` is a complete `Lead`. The response contains
all nine stages, even when a stage has no leads.

### Stage definitions and count

`GET /kanban/stages`

```json
{
  "data": [
    {
      "key": "enquiry",
      "order": 1,
      "defaultSubstate": "new",
      "substates": [
        "new",
        "contact_attempted",
        "contacted",
        "nurture",
        "qualified",
        "converted"
      ]
    }
  ]
}
```

`GET /kanban/stages/{stage}/count`

```json
{
  "data": {
    "stage": "enquiry",
    "count": 12
  }
}
```

### WebSocket event

After the `101 Switching Protocols` response, `/events` emits one JSON message
per committed CRM outbox event:

```json
{
  "cursor": 42,
  "eventType": "lead.moved",
  "aggregateId": "07fb390f-d35d-4570-bd26-8a0573532b03",
  "payload": {
    "leadId": "07fb390f-d35d-4570-bd26-8a0573532b03",
    "fromStage": "enquiry",
    "toStage": "contact_attempted",
    "toSubstate": "contacted",
    "byUser": "admin-001"
  },
  "createdAt": "2026-07-30T00:10:00Z"
}
```

### Form definition

Form create, get, update, publish, and unpublish operations return:

```json
{
  "data": {
    "id": "50bf6f91-80c7-4488-a95e-3d09ed6163cf",
    "name": "Admission Enquiry",
    "formType": "enquiry",
    "programId": null,
    "intakeYear": 2027,
    "version": 1,
    "status": "draft",
    "schema": {
      "sections": [{
        "section": "Primary details",
        "fields": [
          { "key": "student_name", "label": "Student name", "type": "Short text", "required": true },
          { "key": "whatsapp", "label": "WhatsApp number", "type": "Phone", "required": true },
          {
            "key": "course_type",
            "label": "Course type",
            "type": "Dropdown",
            "required": true,
            "placeholder": "Select a course",
            "helpText": "Courses currently accepting applications",
            "options": ["B.Tech Computer Science", "B.Tech Electronics", "MBA"]
          }
        ]
      }],
      "metadata": {
        "module": "CRM",
        "owner": "Tenant Admin",
        "usage": "Create leads from the CRM workspace"
      }
    },
    "createdBy": "admin-001",
    "updatedBy": "admin-001",
    "createdAt": "2026-07-30T00:15:00Z",
    "updatedAt": "2026-07-30T00:15:00Z"
  }
}
```

`GET /forms` returns these objects inside `data: []`.

The publish endpoint rejects dropdown, radio, or multi-select fields with no
configured options. The backend also rejects submitted choice values that do
not exist in the options stored in the submitted form version. A form appears
in CRM Create Lead only when its `formType` is `lead_capture` and its status is
`published`; publishing an Admissions `application` form does not route it to
CRM.

### Form submission

Internal submission:

```json
{
  "data": {
    "id": "55a42885-da68-418b-b92c-4839b44ff393",
    "formId": "50bf6f91-80c7-4488-a95e-3d09ed6163cf",
    "formVersion": 1,
    "leadId": "07fb390f-d35d-4570-bd26-8a0573532b03",
    "data": {
      "name": "Test Student",
      "phone": "9000000001"
    },
    "createdAt": "2026-07-30T00:20:00Z"
  }
}
```

Public enquiry submission additionally returns the automatically created lead:

```json
{
  "data": {
    "id": "55a42885-da68-418b-b92c-4839b44ff393",
    "formId": "50bf6f91-80c7-4488-a95e-3d09ed6163cf",
    "formVersion": 1,
    "leadId": "a2ccb999-db3f-4bb5-8235-189e22802abe",
    "createdLeadId": "a2ccb999-db3f-4bb5-8235-189e22802abe",
    "data": {
      "name": "Public Applicant",
      "phone": "9000000010"
    },
    "createdAt": "2026-07-30T00:21:00Z"
  }
}
```

`GET /forms/{id}/submissions` returns an array of submission objects and also
includes `submittedBy` for each stored submission.

### Communication

WhatsApp, email, and call endpoints return:

```json
{
  "data": {
    "id": "80f90853-d7f3-47ca-9c71-ad360d40cb2e",
    "leadId": "07fb390f-d35d-4570-bd26-8a0573532b03",
    "channel": "whatsapp",
    "direction": "outbound",
    "templateKey": "qualified_confirmation",
    "subject": null,
    "content": {
      "message": "You are qualified for the selected program"
    },
    "outcome": null,
    "status": "queued",
    "actorId": "counselor-001",
    "createdAt": "2026-07-30T00:25:00Z"
  }
}
```

The response confirms that communication was recorded and queued. It does not
mean that the external provider has delivered the message.

### Communication template

```json
{
  "data": {
    "id": "4f628334-f671-410e-9f0c-a31aa0815b7b",
    "templateKey": "qualified_confirmation",
    "channel": "whatsapp",
    "name": "Qualified confirmation",
    "content": "You are qualified for {{program_name}}.",
    "language": "en",
    "status": "draft",
    "updatedAt": "2026-07-30T00:30:00Z"
  }
}
```

Template list endpoints return these objects inside `data: []`.

### Counselor capacity

```json
{
  "data": {
    "userId": "counselor-001",
    "displayName": "Admission Counselor",
    "active": true,
    "maxCapacity": 100,
    "sourceCategories": [],
    "programIds": [],
    "territories": [],
    "averageResponseMinutes": 20.0,
    "conversionRate": 0.4,
    "activeLeads": 8
  }
}
```

The list endpoint returns these objects inside `data: []`.

### CRM configuration

`GET /configuration`

```json
{
  "data": {
    "workflowToggles": [
      {
        "id": "7472a79e-bc6a-4c69-b865-ed71eca72281",
        "fromStage": "enquiry",
        "toStage": "contacted",
        "allowedRoles": ["admissions_manager"],
        "requiresApproval": false,
        "approvalRole": null,
        "enabled": true
      }
    ],
    "automationToggles": [
      {
        "id": "ef45bf3f-7773-4f4c-bc79-c2a63009456d",
        "stage": "qualified",
        "triggerName": "on_enter",
        "action": "send_whatsapp",
        "templateKey": "qualified_confirmation",
        "conditions": [],
        "enabled": true,
        "mandatory": false
      }
    ]
  }
}
```

`PUT /configuration/workflow-toggles` returns one workflow toggle:

```json
{
  "data": {
    "id": "7472a79e-bc6a-4c69-b865-ed71eca72281",
    "fromStage": "enquiry",
    "toStage": "contacted",
    "allowedRoles": ["admissions_manager"],
    "requiresApproval": false,
    "approvalRole": null,
    "enabled": true
  }
}
```

`PUT /configuration/automation-toggles` returns one automation toggle:

```json
{
  "data": {
    "id": "ef45bf3f-7773-4f4c-bc79-c2a63009456d",
    "stage": "qualified",
    "triggerName": "on_enter",
    "action": "send_whatsapp",
    "templateKey": "qualified_confirmation",
    "conditions": [],
    "enabled": true,
    "mandatory": false
  }
}
```

### Delete response

Successful lead and form deletes return:

```http
HTTP/1.1 204 No Content
```

There is no JSON response body for a `204` operation.
## Common response format

Successful responses normally use:

```json
{
  "data": {}
}
```

Errors use:

```json
{
  "error": {
    "code": "validation_error",
    "message": "Description of the error"
  }
}
```

## Common status codes

| Status | Meaning |
|---|---|
| `200 OK` | Request completed |
| `201 Created` | Lead, form, template, or submission created |
| `204 No Content` | Soft-delete completed |
| `400 Bad Request` | Invalid fields, stage, substate, or business condition |
| `401 Unauthorized` | Identity headers are missing |
| `403 Forbidden` | Role, ownership, or tenant workflow policy denied the action |
| `404 Not Found` | Tenant-scoped record does not exist |
| `409 Conflict` | State conflict, including progression while on hold |
| `500 Internal Server Error` | Database or internal operation failed |
| `503 Service Unavailable` | CRM database is unavailable |

## Quick health test

```powershell
Invoke-RestMethod http://localhost:4000/api/v1/crm/health
```

