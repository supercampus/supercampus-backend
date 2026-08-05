# SuperCampus CRM — Roles and Permissions Specification

## 1. Role Architecture

The CRM operates on a strict three-tier hierarchy. Permissions are enforced at entity, stage, field, and action levels.

### 1.1 Role Categories

| Category | Roles | System Access |
|----------|-------|---------------|
| **Applicant** | Prospective Student, Parent, Guardian | None. Receives automated WhatsApp messages only. |
| **Admission Team** | Front Office, Telecaller, Receptionist, Campus Tour Coordinator, Admission Counselor, Senior Admission Counselor, Admissions Manager, Application Reviewer, Document Verification Officer, Admission Committee Member, Interview Panel Member, Program Advisor, Scholarship Officer, Scholarship Committee Approver, Principal Admission Approver, Management Quota Approver, Admission Finance Officer, Admission Cashier | Full CRM access scoped by role tier |
| **Marketing Team** | Marketing Executive, Digital Marketing Manager, Outreach / School Visit Coordinator, Sales Manager | View-only dashboard access |

### 1.2 Permission Dimensions

Every action is validated across five dimensions:

1. **Entity CRUD** — Create, Read, Update, Delete on Leads
2. **Stage Transition** — Move a record from Stage A to Stage B
3. **Field-Level Access** — Which fields are visible or editable
4. **Action Execution** — Send message, trigger automation, archive, assign
5. **Toggle Configuration** — Enable or disable workflow and automation toggles

---

## 2. Applicant Permissions

Applicants have **zero system access**. They do not log into the CRM. They do not fill forms. They do not upload documents. They do not view any pipeline or portal.

| Capability | Access | Method |
|------------|--------|--------|
| Receive stage updates | Outbound only | Automated WhatsApp message |
| Receive document requests | Outbound only | Automated WhatsApp message with instructions |
| Receive interview confirmation | Outbound only | Automated WhatsApp message |
| Receive offer letter | Outbound only | Automated WhatsApp message with link or attachment |
| Receive archive closure | Outbound only | Automated WhatsApp message |
| Enquiry submission | External only | Public enquiry form (creates lead in CRM, no account created) |

---

## 3. Admission Team — Tiered Permissions

### Tier A: Frontline Staff
Roles: Front Office, Telecaller, Receptionist, Campus Tour Coordinator

| Capability | Permission |
|------------|------------|
| Lead | Read, Update limited fields |
| Stage Transition | Enquiry to Contact Attempted to Contacted to Nurture |
| Assignment | Cannot assign or reassign |
| Archive | Cannot archive |
| Communication | Log calls, send WhatsApp templates |
| Form Builder | No access |
| Dashboard | View assigned leads only |
| Toggles | None |

### Tier B: Admission Counselors
Roles: Admission Counselor, Senior Admission Counselor

| Capability | Permission |
|------------|------------|
| Lead | Full CRUD on assigned leads |
| Stage Transition | All Lead stages up to Qualified |
| Assignment | Claim unassigned leads; request reassignment |
| Archive | Cannot archive; can flag for manager review |
| Communication | Send WhatsApp and email; trigger automated messages |
| Form Builder | Read-only view of configured forms |
| Dashboard | Own pipeline; team overview if Senior |
| Prospect / Deferred / On Hold | Can place On Hold; can flag Prospect or Deferred for manager |
| Toggles | None |

### Tier C: Managers and Approvers
Roles: Admissions Manager, Application Reviewer, Document Verification Officer, Admission Committee Member, Interview Panel Member, Program Advisor, Scholarship Officer, Scholarship Committee Approver, Principal Admission Approver, Management Quota Approver, Admission Finance Officer, Admission Cashier

| Capability | Permission |
|------------|------------|
| Lead | Full CRUD on all leads |
| Stage Transition | All stages including Application, Application Status, Offer Status, Archive |
| Assignment | Full reassignment; configure auto-assignment rules |
| Archive | Full archive with reason selection |
| Communication | Full; configure message templates and automation triggers |
| Form Builder | Full access — create, edit, publish forms; set conditional logic |
| Dashboard | Full board, all analytics, team performance |
| Prospect / Deferred / On Hold | Full control |
| Offer Decision | Move to Accepted or Rejected |
| ERP Handoff | Trigger ERP migration for Accepted offers |
| Toggles | Enable or disable workflow toggles and automation toggles |

---

## 4. Marketing Team Permissions

| Capability | Permission |
|------------|------------|
| Lead | Read-only all leads |
| Stage Transition | None |
| Assignment | None |
| Archive | None |
| Communication | Read-only campaign metrics |
| Form Builder | Read-only view of forms |
| Dashboard | View-only unified Kanban board; see pipeline, sources, conversion metrics |
| Reports | Generate marketing source reports |
| Toggles | None |

---

## 5. Dynamic Workflow Toggles

### 5.1 Role-Based Permission Toggles

Each stage transition is gated by role-based toggles configured by the Admissions Manager.

```
Toggle Schema:
{
  "stage_from": "Enquiry",
  "stage_to": "Contact Attempted",
  "allowed_roles": ["Telecaller", "Admission Counselor", "Senior Admission Counselor", "Admissions Manager"],
  "requires_approval": false,
  "approval_role": null
}
```

Default Rules:
- Frontline staff: Enquiry to Contact Attempted to Contacted to Nurture
- Counselors: additionally Nurture to Qualified
- Managers: Qualified to Application to Application Status to Offer Status to Archive
- Only Managers and Principal or Management Approvers: Accepted, Archive

### 5.2 Automation Toggles

Each stage has automation triggers on entry or exit.

```
Automation Toggle Schema:
{
  "stage": "Qualified",
  "trigger": "on_entry",
  "action": "send_whatsapp",
  "template_id": "qualified_congrats",
  "enabled": true,
  "conditions": ["lead_source != 'Spam'"]
}
```

Mandatory Automations (always on, non-toggle):
- Every stage movement after Qualified sends WhatsApp message to applicant
- Application Submitted auto-moves to Application Status: Awaiting Decision
- Offer Accepted auto-triggers ERP handoff
- Offer Rejected auto-moves to Archive

Configurable Automations (toggle-enabled):
- Enquiry created: auto-assign to counselor
- Contact Attempted: auto-schedule follow-up reminder
- Documents Required: auto-send document checklist via WhatsApp
- Interview Scheduled: auto-send interview details via WhatsApp
- Waitlisted: auto-send waitlist notification
- On Hold: auto-reminder after N days

---

## 6. Lead Assignment Rules

### 6.1 Automated Assignment (Digital Sources)

Trigger: Lead created from online or digital sources.

Auto-assignment sources:
AI Search Engine, Bing Search, Google Search, Google Ads, Google My Business, Facebook, Instagram, LinkedIn, Youtube, CollegeDekho, Collegedunia, Shiksha, Careers360, Jagran Josh, MEC Website, Other Aggregated Website, Other Search Engines, Quora Answers, In-Bound Call, In-Bound WhatsApp, SMS Broadcast, Whatsapp Broadcast, Webinars, TNEA Counselling.

Assignment Logic:
1. Filter active counselors with capacity below max limit
2. Apply source-based routing rules
3. Apply program or intake-based routing if specified
4. Apply territory-based routing if pincode or region is captured
5. Score candidates by workload, response time, and conversion rate
6. Select highest score; tie-break by round-robin

### 6.2 Manual Assignment (Offline Sources)

Manual assignment sources:
Walk-In, Education Fair, Seminar, Referral (Alumni, Current Student, Parents, School Counselor), Agent/Consultant, Outbound Calling, Radio.

Manual flow:
1. Front Office or Receptionist creates lead
2. Lead marked as Unassigned
3. Admissions Manager or Senior Counselor manually assigns
4. Assignment notification sent to assigned counselor

### 6.3 Reassignment Rules

- Only Admissions Manager and above can reassign
- Reassignment logs: old owner, new owner, reason, timestamp
- Reassigned lead retains full history

---

## 7. Archive and On Hold Permissions

### 7.1 Archive

| Role | Stage | Requirement |
|------|-------|-------------|
| Admissions Manager, Principal Approver, Management Quota Approver | Any stage | Select one of 31 Archive Reasons |
| System auto-archive | Offer Rejected | Auto-archive with reason No Offer or Offer Expired |

Archive Reasons (31):
Academic Ineligibility, Age Criteria Not Met, Calls Not Answered, Duplicate Lead, Education Gap, Education Loan Rejected, Fake Documents, Financial Ineligibility, Full Scholarship Required, Health Issues, Insufficient Documents, Intake Deadline Passed, Interview No Show, Invalid Number, Lost to Competitor, Low Score, No Offer, No Offer from Preferred Choice, No Revenue Potential, Not Happy with Service, Not Interested in Engineering, Not Reachable, Not Satisfied with Offering, Offer Expired, Others, Program Full/Closed, Program Not Available, Program Not Offered, Refund Initiated, Spam, Student Opted Out.

### 7.2 On Hold

| Role | Stage | Requirement |
|------|-------|-------------|
| Admission Counselor and above | Any stage | Add reason and reminder date |
| System | Auto | Trigger if document pending exceeds N days |

On Hold behavior:
- Record stays in current stage with On Hold status
- No automated stage progression while On Hold
- Counselor receives reminder on reminder date
- Can be removed by same role or higher

### 7.3 Prospect and Deferred

| Role | Condition | Requirement |
|------|-----------|-------------|
| Admission Counselor and above | Prospect | Lead must have crossed Qualified stage in history |
| Admission Counselor and above | Deferred | Can be applied from any stage |

---

## 8. Form Builder Permissions

The Form Builder is an internal tool for the Admission Team. Applicants do not interact with forms directly. Counselors and managers use forms to collect, organize, and process applicant data internally.

| Role | Create | Edit | Publish | Conditional Logic | View Submissions |
|------|--------|------|---------|-------------------|------------------|
| Admissions Manager | Yes | Yes | Yes | Yes | Yes |
| Program Advisor | Yes | Yes (program-specific) | Yes | Yes | Yes |
| Admission Counselor | No | No | No | No | Read-only |
| Marketing Team | No | No | No | No | Read-only (aggregate) |

Form Types:
1. Enquiry Form — Public-facing; creates lead on submit (applicant fills once, no account created)
2. Application Form — Internal form populated by admission staff after Qualification
3. Document Checklist Form — Internal tracking of required documents per program
4. Interview Scheduling Form — Internal slot management
5. Offer Acceptance Form — Internal confirmation of fee and document receipt

---

## 9. Communication Permissions

### 9.1 WhatsApp Automation (Post-Qualified)

Rule: Every stage movement after Qualified sends a WhatsApp message to the applicant.

| Trigger | Default Message |
|---------|-----------------|
| Qualified to Application | Congratulations, you are qualified. Complete your application. |
| Application to In Progress | Your application is under review. |
| Application to Documents Required | Documents pending. Please submit as instructed. |
| Application to Submitted | Application submitted successfully. |
| Interview To Be Scheduled | Interview scheduling required. |
| Interview Scheduled | Interview confirmed for date and time at venue. |
| Unconditional Offer | Offer letter issued. Accept before deadline. |
| Offer Accepted | Welcome. Your admission is confirmed. |
| Offer Rejected | We regret to inform you that your application was not successful. |
| Archive | Your application has been closed. Reason provided. |

Templates are editable by Managers. Each stage has a toggle to enable or disable the automation.

### 9.2 Manual Communication

- Call Logging: Counselor logs outcome (Connected, Not Answered, Wrong Number, Callback Requested)
- WhatsApp: Integration with WhatsApp Business API; template-based messages only
- Email: Rich text editor, attachments, template library
- Communication Timeline: All touchpoints visible on lead record

---

## 10. Special Role Behaviors

### 10.1 Scholarship Officer / Scholarship Committee Approver
- Edit scholarship-related fields
- Approve or reject scholarship applications
- Trigger Full Scholarship Required archive reason

### 10.2 Admission Finance Officer / Admission Cashier
- View fee payment status
- Mark Application Fee Pending as resolved
- Trigger fee-related holds

### 10.3 Document Verification Officer
- Move Application to Documents Required
- Verify uploaded documents
- Trigger Fake Documents or Insufficient Documents archive

### 10.4 Interview Panel Member
- Schedule interviews
- Update interview status (No Show, Completed)
- Add interview scores and notes

### 10.5 Principal Admission Approver / Management Quota Approver
- Override standard pipeline rules
- Approve Management Quota admissions
- Bypass certain stage requirements

---

## 11. Audit and Compliance

- Every permission check is logged: user_id, role, action, entity_id, timestamp, result
- Failed permission attempts trigger alerts to Admissions Manager
- Role changes are versioned; historical permissions preserved
- Data export restricted to Managers and above
