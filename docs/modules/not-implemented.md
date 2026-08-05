# Module HTTP implementation status

A package directory, manifest, migration placeholder or catalog entry does not mean an HTTP API is mounted.

| Module/capability | Package status | Mounted module-specific routes |
|---|---|---|
| CRM | Implemented | Yes, `/api/v1/crm/*` |
| Academics | Scaffold | No |
| Admissions | Scaffold | No; generic `/api/v1/admissions/records` exists |
| Attendance | Scaffold | No |
| Documents | Scaffold | No |
| Examinations | Scaffold | No |
| Fees/Finance | Scaffold | No |
| Gatepass | Scaffold | No |
| Hostel | Scaffold | No |
| Library | Scaffold | No |
| Placement | Scaffold | No |
| Transport | Scaffold | No |
| Users | Not Implemented | No |
| Students | Not Implemented as dedicated API | No; development auth returns a student-shaped profile |
| Teachers/faculty | Not Implemented | No |
| Parents | Not Implemented | No |
| Courses | Not Implemented as dedicated API | No |
| Timetable | Not Implemented | No |
| Assignments | Not Implemented | No |
| HRMS | Not Implemented | No |
| Inventory | Not Implemented | No |
| Notifications API | Not Implemented | No |
| Reports | Not Implemented | No |
| Settings admin API | Not Implemented | No |
| File API | Not Implemented | No |

The generic dynamic-record router supports a registered `module_key` but does not provide module-specific validation, permissions, business logic, tables, events or workflows. Consumers must not treat it as a completed module API.
