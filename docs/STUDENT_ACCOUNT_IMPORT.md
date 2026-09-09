# Student account import

Endpoint: `POST /api/v1/student-master/accounts/import`.
Requires the authenticated tenant's `students.directory.create`,
`authorization.users.create`, and `authorization.users.update` permissions.
The tenant and Student role are server-selected; the file cannot grant roles.

The app admin User management page has a bulk-upload action; the website's
Students page has Bulk upload. Both download a header-only CSV template:

`Name,Roll No,Email,Mobile number,Department,Year,Section,Password`

Use Year 1 for first year. Department must match an existing department code,
name, or UUID. Roll numbers/mobile numbers should be text in Excel. Passwords
must have at least 12 characters and at most 72 UTF-8 bytes. Keep the file private
and distribute passwords individually. Password reset on first login is not
enforced by this change. Photographs are optional and can be added later through
the existing profile flow or the admin Students photo-upload control.

The app accepts CSV and XLSX (first worksheet). The website accepts CSV.
Clients validate up to 1000 rows and submit batches of 25, returning per-email
created/already-imported/failed results. Existing accounts are never updated.
Retries of completed rows are safe. Do not use the old directory-only import
API for account provisioning.

The importer reserves inactive identities, writes the linked tenant Student
Master, then assigns the Student role and activates the login. With separate
control/tenant databases, a failed or interrupted row can leave an inactive
reservation. Retrying the unchanged row resumes it. Inactive accounts created
outside this import, or subsequently disabled accounts, cannot be reactivated
by this flow. Changed pending rows require administrator investigation; no
bulk overwrite or deletion is performed. Logs contain errors, not passwords.

Year/section are stored in the student profile; this change does not create
academic batches, courses, or timetable enrollments. Those remain academic
administration operations, so first-year students are not silently assigned
to an existing second-year class.

Before release, verify against a dedicated test tenant on the deployed database
topology: create two distinct students, authenticate each, verify names/rolls,
verify Student-only access, retry, reject existing emails/rolls and disabled
accounts, and interrupt/retry a pending import. Do not run these writes against
the real student list as a diagnostic test.
