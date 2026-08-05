# SuperCampus CRM — Pipeline Stages and Endpoints Specification

## 1. Pipeline Overview

The Pre-Admission pipeline is a linear-progressive Kanban board with 9 primary columns and 3 global cross-cutting statuses. Each stage contains substates that define the exact position of a lead within that column.

### 1.1 Primary Pipeline Columns (Kanban Board)

| Column Order | Stage Name | Purpose |
|--------------|------------|---------|
| 1 | Enquiry | Initial lead capture and qualification |
| 2 | Contact Attempted | First outreach made |
| 3 | Contacted | Successful contact established |
| 4 | Nurture | Relationship building and counseling |
| 5 | Qualified | Lead meets basic eligibility criteria |
| 6 | Application | Formal application submission and processing |
| 7 | Application Status | Application under committee review |
| 8 | Offer Status | Final admission decision |
| 9 | Archived | Closed leads (won, lost, or expired) |

### 1.2 Global Cross-Cutting Statuses

These statuses apply to a lead regardless of its current primary stage:

| Global Status | Behavior | Stage Restriction |
|---------------|----------|-------------------|
| Prospect (Select Intake) | Lead defers to a specific intake or year | Must have crossed Qualified stage |
| Deferred (Select Intake) | Lead defers application to future intake | None |
| On Hold (Student) | Temporary pause on progression | None |
| Archive | Permanent closure | None |

---

## 2. Stage Definitions and Substates

### Stage 1: Enquiry

Purpose: Capture and classify incoming interest.

| Substate | Description | Entry Condition |
|----------|-------------|-----------------|
| New | Fresh lead just entered the system | Lead created |
| Contact Attempted | First call or message attempted | Manual move or auto-trigger |
| Contacted | Prospect responded to outreach | Manual move |
| Nurture | Requires follow-up and counseling | Manual move |
| Qualified | Meets age, academic, and basic criteria | Manual move by counselor |
| Converted | Ready to apply; moved to Application | Manual move |

Valid Transitions:
```
New -> Contact Attempted -> Contacted -> Nurture -> Qualified -> Converted
New -> Contacted (if inbound)
New -> Nurture (if self-qualified)
Contact Attempted -> Contacted
Contact Attempted -> Nurture
Contacted -> Nurture
Contacted -> Qualified
Nurture -> Qualified
Nurture -> Converted
Qualified -> Converted
```

### Stage 2: Contact Attempted

Purpose: Track leads where initial outreach was made.

| Substate | Description |
|----------|-------------|
| Contacted | Prospect answered; conversation started |
| Nurture | Needs nurturing before qualification |
| Qualified | Directly qualified during first contact |
| Converted | Immediately ready to apply |

Valid Transitions:
```
Contact Attempted -> Contacted
Contact Attempted -> Nurture
Contact Attempted -> Qualified
Contact Attempted -> Converted
```

### Stage 3: Contacted

Purpose: Leads with established two-way communication.

| Substate | Description |
|----------|-------------|
| Nurture | Requires further counseling |
| Qualified | Eligibility confirmed |
| Converted | Ready for application |

Valid Transitions:
```
Contacted -> Nurture
Contacted -> Qualified
Contacted -> Converted
```

### Stage 4: Nurture

Purpose: Long-term relationship building.

| Substate | Description |
|----------|-------------|
| Qualified | After nurturing, meets criteria |
| Converted | Nurtured lead ready to apply |

Valid Transitions:
```
Nurture -> Qualified
Nurture -> Converted
```

### Stage 5: Qualified

Purpose: Eligible leads awaiting application initiation.

| Substate | Description |
|----------|-------------|
| Converted | Application process initiated |

Valid Transition:
```
Qualified -> Converted -> triggers Application: To Do
```

Post-Qualified Rule: Every stage movement from this point forward triggers a WhatsApp message to the applicant.

### Stage 6: Application

Purpose: Formal application lifecycle management.

| Substate | Description | Action on Entry |
|----------|-------------|-----------------|
| To Do | Application form assigned; awaiting submission | Default status on conversion from Qualified |
| Application in Progress | Applicant data being collected by staff | No Action, Just Status |
| Documents Required | Missing mandatory documents | No Action, Just Status |
| Application Fee Pending | Fee not yet paid | No Action, Just Status |
| Application not Open | Intake not yet open for selected program | No Action, Just Status |
| Technical Issue | Portal or technical problem reported | No Action, Just Status |
| Application Submitted | Form, documents, and fee complete | Move to Application Status: Awaiting Decision |

Valid Transitions:
```
To Do -> Application in Progress
To Do -> Documents Required
To Do -> Application Fee Pending
To Do -> Application not Open
To Do -> Technical Issue
Application in Progress -> Documents Required
Application in Progress -> Application Fee Pending
Application in Progress -> Application Submitted
Documents Required -> Application Submitted
Application Fee Pending -> Application Submitted
Application not Open -> To Do (when intake opens)
Technical Issue -> To Do (when resolved)
Application Submitted -> Application Status: Awaiting Decision (AUTO)
```

### Stage 7: Application Status

Purpose: Committee review and decision-making.

| Substate | Description | Action on Entry |
|----------|-------------|-----------------|
| Awaiting Decision | Under review by admission committee | Default status |
| Documents Required | Additional documents requested post-submission | No Action, Just Status |
| Interview To Be Scheduled | Interview required; slot not yet fixed | No Action, Just Status |
| Interview Scheduled | Interview slot confirmed | No Action, Just Status; ask for Interview Details |
| Waitlisted | Qualified but seat not available | No Action, Just Status |
| Unconditional Offer | Approved for admission | Move to Deposit Payment - Arranging Funds |

Valid Transitions:
```
Awaiting Decision -> Documents Required
Awaiting Decision -> Interview To Be Scheduled
Awaiting Decision -> Waitlisted
Awaiting Decision -> Unconditional Offer
Documents Required -> Awaiting Decision
Interview To Be Scheduled -> Interview Scheduled
Interview Scheduled -> Awaiting Decision (post-interview)
Interview Scheduled -> Unconditional Offer
Waitlisted -> Unconditional Offer (if seat opens)
Unconditional Offer -> Offer Status: To Do (AUTO)
```

### Stage 8: Offer Status

Purpose: Final acceptance or rejection tracking.

| Substate | Description | Action on Entry |
|----------|-------------|-----------------|
| To Do | Offer issued; awaiting student response | Default status |
| Accepted | Student accepted the offer | Move to ERP Post Admission |
| Rejected | Student rejected or offer expired | Move to Archive |

Valid Transitions:
```
To Do -> Accepted
To Do -> Rejected
Accepted -> ERP Post Admission (AUTO)
Rejected -> Archive (AUTO)
```

Accepted Condition: Fee payment and document collection must be confirmed to lock the seat before ERP migration.

### Stage 9: Archived

Purpose: Terminal state for all closed leads.

Archive Entry Points:
- From any stage via manual archive with reason
- Auto-archive from Offer Rejected
- Auto-archive from Duplicate Lead detection
- Auto-archive from Spam detection

---

## 3. Global Status Endpoints

### 3.1 Prospect (Select Intake)

Endpoint:
```
POST /leads/{id}/prospect
Body: { intake_year, intake_month, program_id, reason }
```

Rules:
- Validation: Lead must have crossed Qualified stage at least once in history
- Action: Move to Qualified stage; ask for Intake and Year
- Effect: Lead tagged with future intake; remains in pipeline as Prospect
- Notification: WhatsApp message sent to applicant confirming interest registration

### 3.2 Deferred (Select Intake)

Endpoint:
```
POST /leads/{id}/defer
Body: { intake_year, intake_month, program_id, deferral_reason }
```

Rules:
- Can be applied from any stage
- Action: Move to Qualified stage; ask for Intake and Year
- Effect: Lead status changes to Deferred; original stage history preserved
- Notification: WhatsApp message sent confirming deferral
- Auto-Resume: On intake open date, lead auto-moves to Nurture or Qualified

### 3.3 On Hold (Student)

Endpoint:
```
POST /leads/{id}/hold
Body: { hold_reason, hold_until_date, reminder_date }
```

Rules:
- Can be applied from any stage
- Action: No Action, Just Status
- Effect: Lead progression frozen; current stage preserved
- Notification: WhatsApp message sent explaining hold reason
- Auto-Reminder: System reminds counselor on reminder_date
- Release: POST /leads/{id}/release-hold returns to previous stage

### 3.4 Archive

Endpoint:
```
POST /leads/{id}/archive
Body: { archive_reason_id, archive_notes, archived_by }
```

Rules:
- Can be applied from any stage
- Validation: archive_reason_id must be one of 31 valid reasons
- Action: Ask Archive Reason and Move to Archived
- Effect: Lead removed from active Kanban; searchable in Archive view only
- Notification: WhatsApp message sent with closure reason (if not Spam)
- Reversal: Only Managers can unarchive; restores to previous stage with audit log

---

## 4. Endpoint Specification

### 4.1 RESTful Endpoint Design

Base: /api/v1/crm
Auth: Bearer token + Role validation

#### Lead Lifecycle Endpoints

| Method | Endpoint | Description | Auth Role |
|--------|----------|-------------|-----------|
| POST | /leads | Create new lead | System (auto) or Front Office |
| GET | /leads | List leads filtered by stage, owner, source | Any (scoped) |
| GET | /leads/{id} | Get lead details | Any (if owned or viewable) |
| PATCH | /leads/{id} | Update lead fields | Owner or Manager |
| DELETE | /leads/{id} | Soft delete lead | Manager only |
| POST | /leads/{id}/assign | Assign lead to counselor | Manager |
| POST | /leads/{id}/reassign | Reassign lead | Manager |

#### Stage Transition Endpoints

| Method | Endpoint | Description | Body |
|--------|----------|-------------|------|
| POST | /leads/{id}/stage/move | Move lead to next stage | { to_stage, to_substate, reason, notes } |
| POST | /leads/{id}/stage/prospect | Mark as Prospect | { intake_year, program_id } |
| POST | /leads/{id}/stage/defer | Mark as Deferred | { intake_year, program_id, reason } |
| POST | /leads/{id}/stage/hold | Place On Hold | { reason, hold_until } |
| POST | /leads/{id}/stage/release-hold | Remove On Hold | { reason } |
| POST | /leads/{id}/stage/archive | Archive lead | { archive_reason_id, notes } |
| POST | /leads/{id}/stage/unarchive | Restore from Archive | { restore_to_stage, reason } |

#### Kanban Board Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | /kanban/board | Get full Kanban board structure |
| GET | /kanban/stages | List all stages with substates |
| GET | /kanban/stages/{stage_id}/leads | Get leads in specific stage |
| GET | /kanban/stages/{stage_id}/count | Get count of leads per stage |
| GET | /kanban/my-board | Get personalized Kanban (assigned leads only) |

#### Form Builder Endpoints

| Method | Endpoint | Description | Auth |
|--------|----------|-------------|------|
| POST | /forms | Create new form | Manager |
| GET | /forms | List all forms | Any |
| GET | /forms/{id} | Get form schema | Any |
| PUT | /forms/{id} | Update form schema | Manager |
| DELETE | /forms/{id} | Delete form | Manager |
| POST | /forms/{id}/publish | Publish form | Manager |
| POST | /forms/{id}/unpublish | Unpublish form | Manager |
| POST | /forms/{id}/submit | Submit form response | Internal staff |
| GET | /forms/{id}/submissions | Get submissions | Manager or Owner |

#### Communication Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | /communications/whatsapp | Send WhatsApp message | Counselor or above |
| POST | /communications/email | Send email | Counselor or above |
| GET | /communications/templates | List message templates | Any |
| POST | /communications/templates | Create template | Manager |

### 4.2 WebSocket Events (Real-time Kanban)

| Event | Payload | Trigger |
|-------|---------|---------|
| lead.created | Lead object | New lead added |
| lead.moved | { lead_id, from_stage, to_stage, by_user } | Stage transition |
| lead.assigned | { lead_id, to_user_id, by_user_id } | Assignment |
| lead.archived | { lead_id, reason, by_user } | Archive |
| lead.hold | { lead_id, hold_reason, until } | On Hold |
| stage.count_changed | { stage_id, new_count } | Any movement |
| offer.accepted | { lead_id, student_id } | Offer accepted |
| offer.rejected | { lead_id, reason } | Offer rejected |

---

## 5. Validation Rules

### 5.1 Stage Transition Validations

Rule 1: A lead cannot skip stages (for example, Enquiry to Application directly).
Rule 2: Only one active primary stage at a time.
Rule 3: Global statuses overlay but do not replace primary stage.
Rule 4: Archive is terminal. No outbound transitions except unarchive.
Rule 5: Prospect requires history.stage greater than or equal to Qualified.
Rule 6: Application Submitted requires all mandatory documents and fee if applicable.
Rule 7: Unconditional Offer requires interview completion if interview was scheduled.
Rule 8: Accepted requires fee payment confirmation or scholarship approval.

### 5.2 Data Integrity Rules

Rule 9: Every stage move logs timestamp, from_stage, to_stage, user_id, and ip_address.
Rule 10: Archive requires non-null archive_reason_id.
Rule 11: Duplicate leads must be flagged before archive with reason Duplicate Lead.
Rule 12: Lead source is immutable after creation.
Rule 13: Applicant phone and email cannot be modified by staff after verification without manager approval.

---

## 6. Lead Source Classification

### 6.1 Auto-Assignment Sources (Digital)

AI Search Engine, Bing Search, Google Search, Google Ads, Google My Business, Facebook, Instagram, LinkedIn, Youtube, CollegeDekho, Collegedunia, Shiksha, Careers360, Jagran Josh, MEC Website, Other Aggregated Website, Other Search Engines, Quora Answers, In-Bound Call, In-Bound WhatsApp, SMS Broadcast, Whatsapp Broadcast, Webinars, TNEA Counselling.

### 6.2 Manual-Assignment Sources (Offline)

Walk-In, Education Fair, Seminar, Referral (Alumni, Current Student, Parents, School Counselor), Agent/Consultant, Outbound Calling, Radio.

### 6.3 Source-Based Routing Rules

| Source Category | Default Assignment Pool | Special Handling |
|-----------------|------------------------|------------------|
| Education Portal | Counselor round-robin | Source tag preserved for ROI tracking |
| Social Media | Digital Marketing Manager pool then Counselor | UTM parameters captured |
| Search Engine | Counselor round-robin | Search query logged |
| Referral | Manual (Manager assigns) | Referrer name and contact logged |
| Walk-In | Front Office then Manual assign | Walk-in date and time logged |
| Agent/Consultant | Manager assigns with Agent tag | Commission tracking field |
| TNEA Counselling | Specialized counselor pool | Rank and score fields mandatory |
| In-Bound Call or WhatsApp | Auto-assign to available counselor | Call reference logged |

---

## 7. ERP Handoff Specification

### 7.1 Trigger

Event: Offer Status equals Accepted, Fee Payment Confirmed, and Documents Collected.

### 7.2 Handoff Payload

```json
{
  "crm_lead_id": "uuid",
  "student_name": "string",
  "student_email": "string",
  "student_phone": "string",
  "program_id": "uuid",
  "intake_year": "number",
  "intake_month": "string",
  "fee_payment_status": "paid",
  "documents_verified": true,
  "offer_accepted_date": "iso_timestamp",
  "admission_counselor_id": "uuid",
  "archive_reason_if_any": null,
  "lead_source": "string",
  "application_form_data": {},
  "document_urls": [],
  "interview_scores": {},
  "scholarship_status": "none|applied|approved"
}
```

### 7.3 Post-Handoff CRM Behavior

- Lead remains in CRM with status Migrated to ERP
- CRM record becomes read-only for Admission Team
- ERP student ID is written back to CRM
- Post-admission communication happens via ERP
- CRM retains historical data for analytics and reporting

### 7.4 Failure Handling

- If ERP handoff fails: Lead stays in Offer Accepted with error flag
- Retry mechanism: 3 automatic retries with exponential backoff
- Manual retry: Manager can trigger re-handoff from dashboard
- Alert: Admissions Manager and IT Admin notified on persistent failure
