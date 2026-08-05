# SuperCampus CRM — Modules and Complete Flow Specification

## 1. Module Architecture

The CRM is composed of 8 core modules that interact through event-driven architecture. Each module exposes internal APIs and emits domain events consumed by other modules.

```
+-------------------------------------------------------------+
|                    UNIFIED DASHBOARD                          |
|              (Kanban Board — Single Screen)                   |
+-------------------------------------------------------------+
                              |
        +---------------------+---------------------+
        |                     |                     |
        v                     v                     v
+--------------+    +----------------+    +-----------------+
| Lead Capture |    |   Pipeline     |    |  Communication  |
|   Module     |--->|    Module      |<---|    Module       |
+--------------+    +----------------+    +-----------------+
        |                     |                     ^
        v                     v                     |
+--------------+    +----------------+    +-----------------+
|  Assignment  |    |  Form Builder  |    |   Archive and   |
|   Module     |--->|    Module      |    |   Hold Module   |
+--------------+    +----------------+    +-----------------+
        |                     |                     |
        +---------------------+---------------------+
                              |
                              v
                    +----------------+
                    |   ERP Handoff  |
                    |    Module      |
                    +----------------+
```

---

## 2. Module 1: Lead Capture

### 2.1 Purpose
Ingest leads from all 35 sources into the CRM with standardized schema and source attribution.

### 2.2 Input Channels

| Channel | Type | Auto-Create Lead |
|---------|------|------------------|
| Website Enquiry Form | Digital | Yes |
| Landing Pages | Digital | Yes |
| WhatsApp Business API | Digital | Yes |
| Facebook Lead Ads | Digital | Yes |
| Google Ads Extensions | Digital | Yes |
| Education Portal APIs | Digital | Yes |
| Phone Call (Inbound) | Digital | Yes (via IVR or CTI) |
| Email Enquiry | Digital | Yes |
| Walk-In Registration | Offline | Manual (Front Office) |
| Education Fair Kiosk | Offline | Manual |
| Seminar Registration | Offline | Manual |
| Referral Form | Offline | Manual |
| Agent Portal | Offline | Manual |
| Outbound Call Success | Offline | Manual |

### 2.3 Lead Schema

```json
{
  "lead_id": "uuid",
  "source": "enum[35_values]",
  "source_detail": {
    "utm_source": "string",
    "utm_medium": "string",
    "utm_campaign": "string",
    "referrer_name": "string",
    "agent_id": "uuid"
  },
  "student": {
    "name": "string",
    "email": "string",
    "phone": "string",
    "whatsapp": "string",
    "parent_name": "string",
    "parent_phone": "string"
  },
  "academic": {
    "highest_qualification": "string",
    "percentage_cgpa": "number",
    "board_university": "string",
    "year_of_passing": "number"
  },
  "interest": {
    "program_id": "uuid",
    "intake_preference": "string",
    "campus_preference": "string"
  },
  "stage": {
    "current_stage": "enum[9_stages]",
    "current_substate": "enum",
    "global_status": "enum[Prospect|Deferred|On Hold|Archive|null]",
    "stage_history": []
  },
  "assignment": {
    "assigned_to": "uuid",
    "assigned_by": "uuid",
    "assigned_at": "timestamp",
    "assignment_type": "auto|manual"
  },
  "communication": {
    "preferred_channel": "sms|email|whatsapp|call",
    "consent_given": "boolean",
    "last_contact_date": "timestamp"
  },
  "metadata": {
    "created_at": "timestamp",
    "updated_at": "timestamp",
    "created_by": "uuid",
    "ip_address": "string",
    "device": "string"
  }
}
```

### 2.4 Duplicate Detection

Rules:
- Match on Phone number (primary) or Email (secondary)
- If duplicate detected: Flag new lead, notify assigned counselor, suggest merge
- Auto-archive duplicate if older than 90 days with no activity
- Merge preserves oldest creation date, all stage history, all communication logs

### 2.5 Events Emitted

| Event | Payload | Consumer |
|-------|---------|----------|
| lead.created | Lead object | Assignment Module, Pipeline Module |
| lead.duplicate_detected | { original_id, duplicate_id, confidence } | Notification Module |

---

## 3. Module 2: Lead Assignment

### 3.1 Purpose
Distribute leads to the right counselor based on source, program, territory, and workload.

### 3.2 Assignment Engine

Auto-Assignment Algorithm:
```
INPUT: New Lead
OUTPUT: Assigned Counselor ID

STEP 1: Filter active counselors with workload below max capacity
STEP 2: Apply source-based routing rules
STEP 3: Apply program-based filtering if program is specified
STEP 4: Apply territory-based filtering if pincode or region is specified
STEP 5: Score candidates:
  - workload_score = 1 / (active_leads_count + 1)
  - response_score = average response time weighted
  - conversion_score = historical conversion rate
  - total_score = weighted_sum(workload: 0.4, response: 0.3, conversion: 0.3)
STEP 6: Select counselor with highest total_score
STEP 7: Tie-break by round-robin
STEP 8: Assign and notify
```

Manual Assignment Interface:
- Manager dashboard shows Unassigned leads queue and counselor workload chart
- Drag-and-drop assignment
- Bulk assignment: select multiple leads, assign to one counselor
- Reassignment with reason logging

### 3.3 Assignment Rules by Source

| Source | Assignment Type | Default Owner |
|--------|----------------|---------------|
| Walk-In | Manual | Front Office (temporary) then Manager assigns |
| Education Fair | Manual | Event coordinator (temporary) then Manager assigns |
| Seminar | Manual | Event coordinator (temporary) then Manager assigns |
| Referral | Manual | Manager |
| Agent/Consultant | Manual | Manager with Agent tagged |
| Outbound Calling | Manual | Caller if successful |
| Radio | Manual | Manager |
| All Digital Sources | Auto | Assignment Engine |

### 3.4 Events Emitted

| Event | Payload | Consumer |
|-------|---------|----------|
| lead.assigned | { lead_id, counselor_id, type } | Pipeline Module, Communication Module |
| lead.reassigned | { lead_id, old_counselor, new_counselor, reason } | Audit Log, Notification |

---

## 4. Module 3: Pipeline (Kanban Board)

### 4.1 Purpose
Visualize and manage the entire pre-admission lifecycle through a drag-and-drop Kanban interface.

### 4.2 Kanban Board Structure

```
+-----------+-----------+-----------+-----------+-----------+-----------------+------------------+-----------+----------+
|  ENQUIRY  |  CONTACT  | CONTACTED |  NURTURE  | QUALIFIED |   APPLICATION   | APPLICATION      |   OFFER   | ARCHIVED |
|           | ATTEMPTED |           |           |           |                 | STATUS           |  STATUS   |          |
+-----------+-----------+-----------+-----------+-----------+-----------------+------------------+-----------+----------+
| New       | Contacted | Nurture   | Qualified | Converted | To Do           | Awaiting         | To Do     | Closed   |
| Contact   | Nurture   | Qualified | Converted |           | In Progress     |   Decision       | Accepted  |  leads   |
| Attempted | Qualified | Converted |           |           | Documents       | Documents        | Rejected  |  (31     |
| Contacted | Converted |           |           |           |   Required      |   Required       |           | reasons) |
| Nurture   |           |           |           |           | Fee Pending     | Interview To     |           |          |
| Qualified |           |           |           |           | Not Open        |   Be Scheduled   |           |          |
| Converted |           |           |           |           | Technical Issue | Interview        |           |          |
|           |           |           |           |           | Submitted       |   Scheduled      |           |          |
|           |           |           |           |           |                 | Waitlisted       |           |          |
|           |           |           |           |           |                 | Unconditional    |           |          |
|           |           |           |           |           |                 |   Offer          |           |          |
+-----------+-----------+-----------+-----------+-----------+-----------------+------------------+-----------+----------+
```

### 4.3 Card Design

Each Kanban card displays:
- Student name and masked phone number
- Lead source (label tag)
- Assigned counselor (initials or avatar)
- Days in current stage (color badge: green under 3 days, yellow 3 to 7 days, red over 7 days)
- Priority flag (High, Medium, Low)
- Quick actions: Call, WhatsApp, Email, Move, Hold, Archive
- Document status indicator if in Application stage
- Next follow-up date

### 4.4 Drag-and-Drop Rules

On Drag Start:
  1. Validate user has stage_transition permission for this lead
  2. Show valid drop zones (highlight allowed columns)
  3. Block invalid drops (gray out disallowed columns)

On Drop:
  1. Validate transition against stage_transition_matrix
  2. Check role-based toggle permissions
  3. If valid: execute move, log event, trigger automations
  4. If invalid: show error toast, revert card position
  5. If requires approval: move to Pending Approval substate

### 4.5 Global Status Overlay

Kanban cards can have overlay badges:
- PROSPECT — Intake selected, future enrollment (yellow)
- DEFERRED — Deferred to next intake (orange)
- ON HOLD — Progress paused (red)
- PRIORITY — Flagged by counselor or manager

### 4.6 Filtering and Views

| View | Filters | Default For |
|------|---------|-------------|
| My Board | assigned_to = me | Counselors |
| Team Board | assigned_to = my_team | Senior Counselors |
| Full Board | None (all leads) | Managers |
| Source View | group_by = lead_source | Marketing Team |
| Program View | group_by = program_id | Program Advisors |
| Archive View | stage = Archived | Managers |

### 4.7 Events Emitted

| Event | Payload | Consumer |
|-------|---------|----------|
| lead.stage_changed | { lead_id, from, to, by_user } | Communication Module, Audit Log |
| lead.substate_changed | { lead_id, stage, old_sub, new_sub } | Automation Engine |
| stage.count_updated | { stage_id, count } | Dashboard Module |

---

## 5. Module 4: Form Builder

### 5.1 Purpose
Dynamically configure internal forms for data collection, document tracking, and process management without code deployment. Applicants do not interact with the Form Builder. All forms are used internally by admission staff.

### 5.2 Form Types

| Form Type | Used By | Purpose |
|-----------|---------|---------|
| Enquiry Form | Public | Creates lead on submit. No applicant account created. |
| Application Form | Admission Staff | Internal data collection after Qualification |
| Document Checklist | Admission Staff | Track required documents per program and intake |
| Interview Scheduling | Interview Panel | Manage interview slots and assignments |
| Offer Acceptance | Admission Staff | Confirm fee receipt and document verification |

### 5.3 Form Builder Features

Field Types:
- Text, Textarea, Email, Phone, Number, Date, Dropdown, Radio, Checkbox, File Upload
- Section Divider, Instructions Block, Conditional Container

Conditional Logic Engine:
```json
{
  "conditions": [
    {
      "field_id": "program_selected",
      "operator": "equals",
      "value": "B.Tech CSE",
      "action": "show",
      "target_field_ids": ["jee_score", "pcm_percentage"]
    },
    {
      "field_id": "category",
      "operator": "equals",
      "value": "International",
      "action": "show",
      "target_field_ids": ["passport_number", "visa_status", "ielts_score"]
    }
  ]
}
```

Program-Intake Specific Forms:
- Each (program_id, intake_year) can have a unique form schema
- Form versioning: edits create new version; old submissions retain old schema
- Form publishing: Draft to Published to Archived lifecycle

### 5.4 Application Form Flow

```
Lead Qualified
    |
    v
System creates Application record
    |
    v
Admission staff receives notification to collect application data
    |
    v
Staff uses Application Form to populate applicant data internally
    |
    v
Staff requests documents via WhatsApp; tracks in Document Checklist
    |
    v
Applicant submits documents (via WhatsApp or physical drop)
    |
    v
Staff verifies documents in CRM
    |
    v
Fee collected (if applicable); marked in system
    |
    v
Staff marks Application Submitted
    |
    v
Auto-move to Application Status: Awaiting Decision
    |
    v
Admission Team reviews and updates status
```

### 5.5 Events Emitted

| Event | Payload | Consumer |
|-------|---------|----------|
| form.submitted | { form_id, lead_id, submission_data } | Pipeline Module, Document Module |
| form.draft_saved | { form_id, lead_id, progress } | Notification Module |
| document.uploaded | { lead_id, doc_type, url, verified } | Pipeline Module |

---

## 6. Module 5: Communication

### 6.1 Purpose
Manage all applicant touchpoints through automated WhatsApp messages and manual staff communications.

### 6.2 WhatsApp Automation (Post-Qualified)

Mandatory Rule: Every stage movement after Qualified sends a WhatsApp message to the applicant.

Template Engine:
- Variables: {student_name}, {program_name}, {intake_year}, {counselor_name}, {interview_date}, {offer_deadline}
- Multi-language support (English and Regional)
- Template approval: Manager approves before activation

WhatsApp Triggers:

| Trigger Event | Template | Recipient |
|---------------|----------|-----------|
| Qualified to Application | qualified_congrats | Applicant |
| Application to In Progress | application_received | Applicant |
| Application to Documents Required | documents_pending | Applicant |
| Application to Submitted | application_submitted | Applicant |
| Interview To Be Scheduled | interview_pending | Applicant |
| Interview Scheduled | interview_confirmed | Applicant |
| Unconditional Offer | offer_issued | Applicant |
| Offer Accepted | admission_confirmed | Applicant and Parent |
| Offer Rejected | offer_rejected | Applicant |
| Archive | application_closed | Applicant |
| On Hold | hold_notification | Applicant |
| Prospect | intake_registered | Applicant |
| Deferred | deferral_confirmed | Applicant |

### 6.3 Manual Communication

- Call Logging: Counselor logs outcome (Connected, Not Answered, Wrong Number, Callback Requested)
- WhatsApp: Integration with WhatsApp Business API; template messages only
- Email: Rich text editor, attachments, template library
- Communication Timeline: All touchpoints visible on lead record

### 6.4 Events Emitted

| Event | Payload | Consumer |
|-------|---------|----------|
| whatsapp.sent | { lead_id, template, status } | Audit Log |
| call.logged | { lead_id, duration, outcome } | Pipeline Module |
| email.sent | { lead_id, subject, opened } | Engagement Score |

---

## 7. Module 6: Archive and Hold

### 7.1 Archive Module

Archive Reasons (31):
1. Academic Ineligibility
2. Age Criteria Not Met
3. Calls Not Answered
4. Duplicate Lead
5. Education Gap
6. Education Loan Rejected
7. Fake Documents
8. Financial Ineligibility
9. Full Scholarship Required
10. Health Issues
11. Insufficient Documents
12. Intake Deadline Passed
13. Interview No Show
14. Invalid Number
15. Lost to Competitor
16. Low Score
17. No Offer
18. No Offer from Preferred Choice
19. No Revenue Potential
20. Not Happy with Service
21. Not Interested in Engineering
22. Not Reachable
23. Not Satisfied with Offering
24. Offer Expired
25. Others
26. Program Full/Closed
27. Program Not Available
28. Program Not Offered
29. Refund Initiated
30. Spam
31. Student Opted Out

Archive Workflow:
```
User clicks Archive
    |
    v
System shows modal: Select Archive Reason (dropdown of 31)
    |
    v
User selects reason and adds notes
    |
    v
Validation: reason_id in valid set
    |
    v
Lead moved to Archived column
    |
    v
WhatsApp message sent to applicant (if reason is not Spam)
    |
    v
Lead removed from active Kanban
    |
    v
Audit log created
```

Unarchive Workflow:
- Only Managers can unarchive
- Restore to previous stage or specify new stage
- Reason required for unarchive
- Notification sent to assigned counselor

### 7.2 On Hold Module

Hold Workflow:
```
User clicks On Hold
    |
    v
System shows modal: Reason, Hold Until Date, Reminder Date
    |
    v
Lead gets On Hold badge on current stage
    |
    v
Progression rules suspended
    |
    v
WhatsApp message sent to applicant
    |
    v
Reminder triggered on reminder_date
    |
    v
Auto-release on hold_until_date (optional)
```

Hold Release:
- Manual: Counselor clicks Release Hold
- Auto: System releases on hold_until_date
- On release: Lead returns to previous stage, progression resumes

---

## 8. Module 7: ERP Handoff

### 8.1 Purpose
Migrate admitted students from CRM to ERP system.

### 8.2 Trigger Conditions

All must be true:
1. Offer Status equals Accepted
2. Fee payment status equals Paid (or Scholarship equals Approved)
3. All mandatory documents are Verified
4. No active holds or disputes

### 8.3 Handoff Process

```
Offer Accepted
    |
    v
Finance Officer confirms payment
    |
    v
Document Officer confirms all documents verified
    |
    v
Manager clicks Confirm Admission -> triggers handoff
    |
    v
System builds ERP payload (student profile, academic, application, documents)
    |
    v
POST /erp/admissions/create with payload
    |
    v
ERP responds with student_id and enrollment_number
    |
    v
CRM stores ERP reference: erp_student_id, erp_enrollment_number
    |
    v
CRM lead status updated to Migrated to ERP
    |
    v
CRM record becomes read-only for Admission Team
    |
    v
Welcome WhatsApp message sent
    |
    v
Dashboard updates: Seat Confirmed counter increments
```

### 8.4 Failure and Retry

| Scenario | Action |
|----------|--------|
| ERP timeout | Retry 1: immediate; Retry 2: 5 minutes; Retry 3: 15 minutes |
| ERP validation error | Flag lead, notify Manager and IT, hold in Accepted |
| Duplicate in ERP | Match existing ERP ID, update CRM reference |
| Network failure | Queue in outbox, retry with exponential backoff |

### 8.5 Post-Handoff CRM Access

- Admission Team: Read-only view of migrated record
- Finance: Can view payment history
- Manager: Can view full history for audit
- Marketing: Included in conversion attribution

---

## 9. Module 8: Unified Dashboard

### 9.1 Purpose
Single-screen Kanban board view of all CRM activity, personalized by role.

### 9.2 Dashboard Design

The dashboard is a clean Kanban board. No metrics carousel. No bottom widgets. No notification panels. Just the board, columns, and cards.

```
+-----------------------------------------------------------------------------+
|  TOP BAR                                                                      |
|  [Logo]  Board Title          Breadcrumb  [Visibility] [Filter] [+ New List] |
+-----------------------------------------------------------------------------+
|                                                                               |
|  LEFT SIDEBAR (Icon Navigation)                                               |
|  +----+                                                                       |
|  |Boards|                                                                     |
|  +----+                                                                       |
|  |Search|                                                                     |
|  +----+                                                                       |
|  |Forms |                                                                     |
|  +----+                                                                       |
|  |Comm  |                                                                     |
|  +----+                                                                       |
|  |Users |  (Manager only)                                                     |
|  +----+                                                                       |
|  |Settings| (Manager only)                                                    |
|  +----+                                                                       |
|                                                                               |
|  MAIN CONTENT: KANBAN BOARD ONLY                                              |
|  +---------+ +---------+ +---------+ +---------+ +---------+ +---------+     |
|  | ENQUIRY | |CONTACT  | |CONTACTED| | NURTURE | |QUALIFIED| |APPLICATION|   |
|  |   (12)  | |ATTEMPTED| |   (8)   | |  (15)   | |   (20)  | |   (18)   |   |
|  |         | |   (5)   | |         | |         | |         | |          |   |
|  | [Cards] | | [Cards] | | [Cards] | | [Cards] | | [Cards] | |  [Cards] |   |
|  +---------+ +---------+ +---------+ +---------+ +---------+ +---------+     |
|  +---------+ +---------+                                                     |
|  |APP STATUS| | OFFER  |                                                     |
|  |   (10)   | |  (5)   |                                                     |
|  |          | |        |                                                     |
|  | [Cards]  | | [Cards]|                                                     |
|  +---------+ +---------+                                                     |
|                                                                               |
+-----------------------------------------------------------------------------+
```

### 9.3 Role-Based Dashboard Differences

| Feature | Admission Team | Marketing Team | Manager |
|---------|---------------|----------------|---------|
| Kanban Edit | Yes (scoped) | No (view only) | Yes (all) |
| Lead Create | Yes | No | Yes |
| Lead Delete | No | No | Yes |
| Stage Move | Yes (per toggle) | No | Yes |
| Archive | No (flag only) | No | Yes |
| Form Builder | Read-only | Read-only | Full |
| Settings | No | No | Yes |
| User Management | No | No | Yes |

### 9.4 Real-Time Updates

- WebSocket connection for live Kanban updates
- Push notifications for assignments and stage moves (browser only, no sound)
- Auto-refresh counters every 30 seconds

---

## 10. Complete User Flows

### Flow 1: Digital Lead to Admission

```
[Student searches on Google] -> Clicks ad -> Lands on enquiry form
    |
    v
[Enquiry Form submitted] -> Lead Capture Module creates lead
    |
    v
[Assignment Module] -> Auto-assigns to Counselor A (round-robin)
    |
    v
[Pipeline: Enquiry/New] -> Counselor A sees card on Kanban
    |
    v
Counselor A calls -> [Pipeline: Contact Attempted]
    |
    v
Student answers -> [Pipeline: Contacted] -> Log call
    |
    v
Counselor sends info -> [Pipeline: Nurture]
    |
    v
Student meets criteria -> [Pipeline: Qualified] -> WhatsApp sent automatically
    |
    v
Staff fills Application Form internally -> [Pipeline: Application/To Do -> In Progress]
    |
    v
Staff requests documents via WhatsApp -> [Pipeline: Application/Documents Required]
    |
    v
Documents received and verified -> [Pipeline: Application/Submitted]
    |
    v
Auto-move -> [Pipeline: Application Status/Awaiting Decision]
    |
    v
Committee reviews -> [Pipeline: Interview To Be Scheduled]
    |
    v
Interview conducted -> [Pipeline: Interview Scheduled -> Awaiting Decision]
    |
    v
Approved -> [Pipeline: Unconditional Offer] -> WhatsApp sent
    |
    v
Student accepts -> [Pipeline: Offer Status/Accepted]
    |
    v
Finance confirms payment -> [ERP Handoff Module] -> Migrated to ERP
    |
    v
CRM record locked; Student becomes ERP active
```

### Flow 2: Walk-In Lead to Admission

```
[Student visits campus] -> Front Office registers manually
    |
    v
[Lead Capture] -> Source = Walk-In; Status = Enquiry/New
    |
    v
[Assignment] -> Manager manually assigns to Counselor B
    |
    v
Counselor B takes campus tour -> [Pipeline: Contacted]
    |
    v
Counselor B qualifies -> [Pipeline: Qualified]
    |
    v
Rest of flow same as Flow 1
```

### Flow 3: Referral Lead to Archive

```
[Alumni refers student] -> Manager creates lead (Source = Referral - Alumni)
    |
    v
Assigned to Counselor C
    |
    v
Counselor C attempts contact -> No response (3 attempts)
    |
    v
[Pipeline: Contact Attempted] -> Flag for Archive
    |
    v
Manager reviews -> Archives with reason Calls Not Answered
    |
    v
WhatsApp sent -> Lead removed from active board
```

### Flow 4: Prospect (Deferred Intake)

```
[Student enquires for 2026 intake] -> Lead created, assigned
    |
    v
Counselor qualifies -> [Pipeline: Qualified]
    |
    v
Student says I want to join next year -> Counselor clicks Prospect
    |
    v
System validates: stage >= Qualified -> OK
    |
    v
Lead tagged: Prospect (Intake 2026, Program B.Tech CSE)
    |
    v
Lead remains in Qualified column with Prospect badge
    |
    v
WhatsApp sent: Your interest for 2026 intake is registered
    |
    v
On January 1, 2026 -> Auto-move to Nurture/Qualified for new cycle
```

### Flow 5: On Hold to Resume

```
[Lead in Application/Documents Required]
    |
    v
Student requests delay due to health -> Counselor clicks On Hold
    |
    v
Reason: Health Issues; Hold Until: 30 days; Reminder: 15 days
    |
    v
Lead gets On Hold badge; progression frozen
    |
    v
Day 15: Reminder to Counselor
    |
    v
Day 30: Auto-release -> Returns to Application/Documents Required
    |
    v
Counselor follows up -> Documents received -> Flow continues
```
