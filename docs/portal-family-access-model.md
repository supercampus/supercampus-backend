# Portal Family Access Model

This note captures the agreed direction for SuperCampus portal access. The goal is
to keep the product understandable for every college while still letting each
institution configure its own roles and module access.
 
## Core Idea

SuperCampus should use fixed portal shells and dynamic permissions inside those
shells.

```text
Portal family decides WHERE the user lands.
Permissions decide WHAT the user can see and do there.
Scopes decide WHOSE data the user can act on.
```

The base portal families are:

```text
student
parent
staff
admin
```

These are product-level UX categories, not college-specific role names.

## Why Not Only Student, Staff, Parent, Admin Modules?

A fixed portal per user type is useful, but it is not enough by itself.

For example, `staff` is too broad. A librarian, timetable coordinator, faculty
member, hostel warden, accountant, and transport manager are all staff, but they
need very different tools.

Without dynamic permissions, the system would need many hardcoded portals:

```text
library-staff portal
timetable-staff portal
hostel-staff portal
exam-staff portal
finance-staff portal
transport-staff portal
```

That does not scale across colleges because each institution names and combines
roles differently.

With dynamic permissions, SuperCampus keeps one staff portal and lets each
college configure which staff roles can access which features.

## Data Model Concept

```text
User
  -> has one or more roles
  -> each role belongs to a portal family
  -> roles grant permissions
  -> permissions unlock module features and actions
  -> scopes restrict the data boundary
```

Example:

```text
User: Ravi
Role: Timetable Coordinator
Portal family: staff
Permissions:
  - timetable.config.update
  - timetable.schedule.create
  - timetable.schedule.update
  - timetable.publication.publish
Scope: institution
```

Ravi logs into the staff portal and sees the timetable allocator.

Another example:

```text
User: Asha
Role: Student
Portal family: student
Permissions:
  - timetable.schedule.read
  - library.slot_booking.create
  - library.book_request.create
  - gatepass.outpass.create
Scope: own
```

Asha logs into the student portal and sees student workflows only.

## Role Names Can Vary By College

College-specific role names should not control the portal directly.

```text
College A: Librarian
College B: Library Incharge
College C: Knowledge Centre Staff
```

All three can still map to:

```text
portal_family = staff
permissions:
  - library.student_qr.verify
  - library.issue.create
  - library.return.create
  - library.request.approve
```

This keeps the system flexible without hardcoding every college's terminology.

## Portal Family Versus Permission

Portal family is a stable UX decision:

```text
student -> student app/dashboard
parent  -> parent app/dashboard
staff   -> operational staff dashboard
admin   -> institution configuration dashboard
```

Permissions are configurable capabilities:

```text
library.slot_booking.create
library.student_qr.verify
timetable.schedule.create
timetable.publication.publish
authorization.roles.update
```

The frontend should not treat a module grant as full access. A module grant only
means the module can appear. Feature permissions decide the exact workflows.

## Scope Model

Permissions need scopes. CRUD alone is not enough.

Recommended minimum scopes:

```text
own
assigned
department
institution
all
```

Examples:

```text
Student + attendance.read + own
  -> can view only their attendance.

Faculty + attendance.read + assigned
  -> can view assigned classes.

HOD + attendance.read + department
  -> can view department attendance.

Admin + attendance.read + institution
  -> can view institution-wide attendance.
```

## Library Example

The Library module should not expose the same generic CRUD workflow to students
and staff. They share the module, but not the same feature permissions.

Student-facing Library permissions:

```text
library.catalog.search.read
library.slot_booking.create
library.slot_booking.read
library.book_request.create
library.book_request.read
library.qr_pass.read
library.visit_history.read
```

Library staff permissions:

```text
library.catalog.create
library.catalog.read
library.catalog.update
library.catalog.delete
library.student_qr.verify
library.issue.create
library.return.create
library.book_request.approve
library.book_request.reject
library.occupancy.read
library.occupancy.update
library.fines.read
library.fines.update
```

Admin Library permissions:

```text
library.settings.update
library.policy.update
library.catalog.delete
library.reports.read
```

Student portal behavior:

```text
Search books
Book library slot
Request book
Show QR pass
View own visit history
```

Staff portal behavior:

```text
Scan student QR
Validate library visit
Approve or reject book requests
Issue and return books
Manage catalog
View occupancy
```

## Timetable Example

Student timetable access:

```text
timetable.schedule.read
Scope: own
```

Faculty timetable access:

```text
timetable.schedule.read
timetable.substitution.create
Scope: assigned
```

Timetable allocator access:

```text
timetable.config.create
timetable.config.read
timetable.config.update
timetable.schedule.create
timetable.schedule.read
timetable.schedule.update
timetable.schedule.delete
timetable.publication.publish
Scope: department or institution
```

Student portal behavior:

```text
View weekly timetable
View timetable changes
```

Staff allocator behavior:

```text
Configure subjects
Set weekly hours
Assign faculty
Allocate rooms
Resolve conflicts
Publish timetable
```

## Multiple Portal Families

A user may belong to more than one portal family.

Example:

```text
User: Meena
Roles:
  - Faculty, portal_family = staff
  - Evening Programme Student, portal_family = student
```

In this case, login should show a portal switcher or open the user's preferred
default portal with an option to switch.

## Admin Portal

The admin portal should be separate from the staff portal.

```text
staff = daily operational work
admin = tenant configuration and access control
```

A user can have both staff and admin access, but they should be separate
workspaces because the mental model is different.

Admin portal examples:

```text
Create users
Assign roles
Configure modules
Configure feature permissions
Manage institution branding
Manage workflow settings
```

## UX Rule For Access Setup

The access setup UI should avoid showing only technical CRUD labels to normal
college administrators.

Internally the system can store:

```text
create
read
update
delete
approve
verify
publish
```

But the UI should prefer workflow labels:

```text
Book slot
Search catalog
Scan student QR
Approve book request
Create timetable
Publish timetable
Assign faculty
```

CRUD can still be shown as a compact technical summary, but the primary label
should describe the real college workflow.

## Implementation Direction

1. Add `portal_family` to roles or memberships.
2. Keep route shells fixed: student, parent, staff, admin.
3. Expand permission templates so feature keys represent real workflows.
4. Add scopes to every grant and enforce them in backend services.
5. Change the frontend access setup UI to group permissions by workflow labels.
6. Render module pages from effective permissions, not just role names.
7. Keep backend authorization mandatory for every action.

## Final Rule

```text
Do not make portals fully dynamic.
Make portal shells fixed and make capabilities dynamic.
```

This gives every college a familiar base structure while still allowing
institution-specific roles and access control.
