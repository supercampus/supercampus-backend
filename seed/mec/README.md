# Madras Engineering College seed

A complete institution on the `mec` tenant: 245 accounts, six departments, a
roll of 200, three campus shops, and the roles and grants that separate them.

Account list and the scope ladder: [CREDENTIALS.md](CREDENTIALS.md).

## Layout

The platform splits its data across two databases and this seed respects that
split, because authentication and authorisation read one of them and the campus
reads the other:

| database | holds | seeded by |
| --- | --- | --- |
| `SuperCampusControl` | `identity.users`, `identity.tenant_memberships`, `authz.*`, `platform.tenants` | `01_control.sql` |
| `MecCampus` | `core.*`, `campus_ops.*`, and an `identity.users` mirror | `02_campus.sql` |

A person exists in both under one id. The control row is what logs in; the
tenant row is what `core.students.user_account_id` and `core.employees.user_id`
point at, since those foreign keys resolve inside the tenant database.

## Running it

Both SQL files are generated, and both are idempotent — every id is a `uuid5`
of a fixed namespace, so re-applying updates rather than duplicates.

```sh
python seed/mec/generate_seed.py

psql "$CONTROL_DATABASE_URL"                   -f seed/mec/01_control.sql
psql "${CONTROL_DATABASE_URL%/*}/MecCampus"    -f seed/mec/02_campus.sql
```

`01_control.sql` ends with a query listing any permission key it asked for that
this tenant does not define. An empty result is the expected outcome.

To change the shared password, edit `PASSWORD` at the top of the generator and
re-run both files.

### Provisioning from scratch

If `MecCampus` does not exist yet:

```sh
cargo run -p supercampus-migration-runner -- provision mec MecCampus
```

That creates the database, migrates it, copies the tenant row in and registers
it in `platform.tenant_databases`.

## What the dataset contains

- **6 departments** — AIDS, CSBS, IT, CYBER, CSE, AIML — each with a programme,
  a 2026–2030 batch, one section, five subjects and five offerings.
- **200 students**, spread 33/34 per department, 100 hostellers and 100 day
  scholars. Residency is assigned by taking every second student of each gender,
  so the hostels draw evenly from all six departments rather than filling from
  the alphabetically first ones.
- **20 staff** — 1 principal, 6 HODs, 6 class advisors, 7 faculty. Advisors and
  HODs are faculty carrying an extra role, never a role of their own.
- **30 teaching assignments**, one per offering. These are what make `assigned`
  scope resolve: a faculty member reaches a section because they teach an
  offering in it. A section-scoped account with no teaching assignment sees an
  empty roster and no error.
- **3 shops** with 3 owners and 7 captains, wired through
  `campus_ops.shop_user_assignments`.
- **13 roles** plus the `tenant_admin` the platform bootstraps.

## Two things this seed does not do

**Hostels have no backend.** The spec called for two hostels of thirty rooms
with four heads to a room and a warden each. No hostel table exists in any
schema, there are no hostel routes, and `HostelRepository` has only a mock
implementation. `core.rooms` is classrooms — it carries `campus_id`,
`department_id` and `room_type`. So hostel membership is recorded on
`core.students.profile` (`hostel`, `room`, `residency`) and the two wardens are
seeded as accounts, but there is nothing to seed the hostels *into*. The data
is shaped to drop into a real schema when the module lands: 50 residents per
hostel, four to a room, rooms 101 upward, leaving 17 of the 30 rooms per hostel
empty.

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
