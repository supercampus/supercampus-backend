# Madras Engineering College seed

A complete institution on the `mec` tenant: 245 active accounts, six
departments, the 201-student MEC25 roll, three campus shops, and the roles and
grants that separate them.

The generated private account list is written to
`seed/mec/source/generated/CREDENTIALS.md`; the tracked `CREDENTIALS.md` remains
the non-production demo reference.

## Layout

The platform splits its data across two databases and this seed respects that
split, because authentication and authorisation read one of them and the campus
reads the other:

| database | holds | seeded by |
| --- | --- | --- |
| `SuperCampusControl` | `identity.users`, `identity.tenant_memberships`, `authz.*`, `platform.tenants` | private generated `01_control.sql` |
| `MecCampus` | `core.*`, `campus_ops.*`, and an `identity.users` mirror | private generated `02_campus.sql` |

A person exists in both under one id. The control row is what logs in; the
tenant row is what `core.students.user_account_id` and `core.employees.user_id`
point at, since those foreign keys resolve inside the tenant database.

## Running it

The roster contains personal data and is deliberately excluded by `.gitignore`.
Prepare it from the institution exports, then generate the two idempotent SQL
files. Every id is a `uuid5` of a fixed namespace, so re-applying updates rather
than duplicates.

```powershell
python seed/mec/prepare_roster.py `
  "C:\path\to\MEC-Students.csv" `
  "C:\path\to\MEC-STUDENTS-IMAGES.zip"

python seed/mec/generate_seed.py

psql "$env:CONTROL_DATABASE_URL" -f seed/mec/source/generated/01_control.sql
psql "<MecCampus connection URL>" -f seed/mec/source/generated/02_campus.sql
```

`01_control.sql` ends with a query listing any permission key it asked for that
this tenant does not define. An empty result is the expected outcome.

Upload the prepared photographs after the roster is live. The uploader prefers
the authenticated media endpoint. When `CLOUDINARY_URL` is supplied, it can use
the same tenant-scoped signed Cloudinary flow directly and emits private SQL to
sync the resulting URLs into both databases.

```powershell
$env:MEC_SEED_PASSWORD = '<temporary password>'
python seed/mec/upload_student_photos.py --api-base https://api.supercampus.ai
```

Student usernames accept either their email address or mobile number. Initial
passwords use the first four letters of the student's name in uppercase plus
the last four mobile-number digits (`Vishnu S` and `1234567890` becomes
`VISH7890`). Apply `source/generated/05_student_credentials.sql` after changing
the roster. The `PASSWORD` constant remains only for non-student demo accounts.

### Provisioning from scratch

If `MecCampus` does not exist yet:

```sh
cargo run -p supercampus-migration-runner -- provision mec MecCampus
```

That creates the database, migrates it, copies the tenant row in and registers
it in `platform.tenant_databases`.

## What the dataset contains

- **6 departments** — AIDS, CSBS, IT, CYBER, CSE, AIML — each with its supplied
  programme name, a 2025–2029 batch, one section, five subjects and five offerings.
- **201 students**, distributed 51/18/33/23/42/34 across AIDS, CSBS, IT, CYBER,
  CSE and AIML. All supplied register numbers, names, phone numbers and email
  addresses are preserved.
- **197 student photographs** matched within departments and canonicalised by
  register number. Four students have no trustworthy image match and remain
  without a photograph rather than receiving someone else's image.
- **19 staff** — 1 principal, 6 HODs, 5 class-advisor accounts covering all 6
  departments, and 7 faculty. Hari Rama Krishna covers CSE and Cyber. Advisors and
  HODs are faculty carrying an extra role, never a role of their own.
- **30 teaching assignments**, one per offering. These are what make `assigned`
  scope resolve: a faculty member reaches a section because they teach an
  offering in it. A section-scoped account with no teaching assignment sees an
  empty roster and no error.
- **3 shops** with 3 owners and 7 captains, wired through
  `campus_ops.shop_user_assignments`.
- **13 roles** plus the `tenant_admin` the platform bootstraps.

## Current limitations

**Hostels have no backend.** The spec called for two hostels of thirty rooms
with four heads to a room and a warden each. No hostel table exists in any
schema, there are no hostel routes, and `HostelRepository` has only a mock
implementation. `core.rooms` is classrooms — it carries `campus_id`,
`department_id` and `room_type`. The source roster contains no residence data,
so student residence is recorded as `unassigned`; no gender, hostel or room is
inferred from names or row order. The two warden demo accounts remain available.

**Six academics features are not grantable.** `elective`, `registration`,
`mentoring`, `warning`, `progress` and `eligibility` exist in the Flutter
catalog but not in `authz.permission_definitions`, so no role here can be
granted them. Class advisors get their approval authority through
`attendance.leave.approve` and `gatepass.outpass.approve` instead. See §10.2 of
`ACADEMIC_MANAGEMENT_REQUIREMENTS.md`.

## Permission keys

`authz.permission_definitions.permission_key` is its own column and does not
always equal `module_key.feature_key.action` — `academics.assignments.manage`,
`canteen.orders.manage`, `fees.refunds.prepare` and `students.status.suspend`
all break that pattern. Grants are inserted by joining against
`permission_definitions`, which drops unknown keys silently rather than failing,
so the generator validates every requested key against
`permission_keys.txt` first and refuses to run if one is unknown.

Refresh that list after changing the permission templates:

```sh
psql "$CONTROL_DATABASE_URL" -qtAX \
  -c "SELECT permission_key FROM authz.permission_definitions d
      JOIN platform.tenants t ON t.id = d.tenant_id AND t.slug = 'mec'
      ORDER BY 1;" > seed/mec/permission_keys.txt
```
