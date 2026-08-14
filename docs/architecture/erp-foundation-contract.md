# SuperCampus ERP Foundation & Module Integration Contract

Status: **locked foundation contract**  
Scope: all backend services, web/mobile clients, workers, and future ERP modules.

## 1. Entrance into ERP

The application review and ERP handoff boundaries are:

```text
CRM Lead -> Application Submitted -> Application Status -> Offer Accepted
                                                                  |
                                                                  v
                                                        Admission Desk Review
                                  |
                         Confirm Onboarding
                                  |
                  Student Master + Enrollment
```

Application submission advances CRM review but does not create an Admission Desk
case or an ERP student. Offer acceptance creates the one authoritative Admission
Desk case used for verification, approval, finance confirmation, and onboarding.

The Offer Accepted transition:

- is validated by the CRM pipeline;
- creates one tenant-scoped review/onboarding case through the Admission Desk service;
- uses the existing transaction, audit, and outbox infrastructure;
- carries lead, submitted form, applicant, guardian, source, owner, custom fields,
  and academic references already known;
- is idempotent across lead, application, offer, and live onboarding references;
- requires the existing Dynamic RBAC permissions.

Repeated offer-acceptance delivery reuses the same case and adds conversion
traceability; it never opens a second case or creates a shadow application inside CRM.

## 2. Canonical ownership

| Domain | Canonical records it owns |
|---|---|
| Core Administration | Institution, campus, academic year, term, department, programme, batch, section |
| Student | Student Master, guardian relationship, academic enrollment, student status |
| Employee | Employee Master and institutional relationship |
| Admission Desk | Admission onboarding case and its authoritative workflow history |
| CRM | Lead, application/offer progression, communications and conversion trace |
| Library | Books, copies, loans, returns, library fines and visits |
| Hostel | Hostels, rooms, beds and allocations |
| Attendance | Sessions, attendance records and corrections |
| Fees & Finance | Structures, assignments, dues, invoices, payments, refunds and concessions |

A domain may store a canonical ID owned by another domain. It may not create a
second profile or shadow master for that record.

## 3. Canonical IDs

All persisted cross-domain relationships use IDs, not display names, email
addresses, phone numbers, admission numbers, or enrollment numbers:

- `tenant_id`
- `campus_id`
- `student_id`
- `enrollment_id`
- `employee_id`
- `academic_year_id`
- `term_id`
- `department_id`
- `programme_id`
- `batch_id`
- `section_id`
- `user_id`
- `guardian_id`

Human-readable numbers remain alternate identifiers and are tenant-unique where
appropriate.

## 4. Student and enrollment

Student Master answers **who the student is**. Academic Enrollment answers
**what the student is studying in a specific academic context**.

Activation creates or reuses one Student Master and creates the corresponding
Academic Enrollment. A later year, term, programme, batch, or section change is
an enrollment mutation; it does not create another Student Master.

Application/offer acceptance creates onboarding only. No active Student Master
exists until all configured onboarding guards are satisfied and the authorized
operator confirms onboarding.

## 5. Identity mappings

User Account, Student Master, Employee Master, and Guardian are distinct.

- A Student may map to one user account.
- An Employee may map to one user account.
- A Guardian may optionally map to one user account.
- A user account does not imply any of those domain identities.
- Student/guardian relationships are explicit records.

## 6. Tenant isolation

Every shared and module-owned table carries `tenant_id`. All relationships
include tenant context, services set `app.tenant_id` inside transactions, and
PostgreSQL row-level security enforces the same boundary.

Cross-tenant foreign relationships are forbidden even when UUIDs are known.

## 7. Authorization

Every module uses the existing Dynamic RBAC decision model:

```text
tenant + user + permission + scope + record context -> allowed action
```

Modules must not implement independent role systems. Service/API authorization
is mandatory; hiding a control in the UI is not authorization.

## 8. Communication

Cross-domain changes use existing service boundaries and the transactional
outbox. Modules must not update another module's private tables.

Foundation event names follow the existing past-tense convention:

- `OnboardingCreated`
- `StudentCreated`
- `StudentActivated`
- `AcademicEnrollmentCreated`
- `AcademicEnrollmentUpdated`
- `StudentProfileUpdated`
- `StudentStatusChanged`
- `SectionChanged`
- `AcademicYearActivated`
- `TermActivated`
- `EmployeeCreated`
- `EmployeeStatusChanged`

New events extend the existing outbox; they do not introduce a competing bus.

## 9. Audit

Important mutations use the shared append-only audit/history conventions and
record tenant, actor, action, aggregate, previous state, next state, reason, and
timestamp. This includes onboarding, document verification, student status,
enrollment/section changes, employee status, finance assignment/correction,
hostel allocation, library fine adjustment, and attendance correction.

## 10. Module acceptance rule

A module is not ready if it:

- creates its own Student, Employee, Programme, Section, Academic Year, or Term;
- relates records by name, email, phone, admission number, or enrollment number;
- omits tenant context;
- bypasses Dynamic RBAC;
- writes directly to another domain's private tables;
- publishes outside the established transactional outbox;
- creates an unrelated audit subsystem.

The implementation sequence after this contract is Core Administration, Student
Onboarding/Student Master, Academic Management, Fees & Finance, Attendance,
Timetable, Communication, Documents, and self-service modules.

