"""Generates the Madras Engineering College dataset as two SQL files.

The platform splits its data across two databases and the seed has to respect
that split exactly: identity and authorisation live in the control plane
(SuperCampusControl), while the campus itself -- departments, sections,
students, employees, shops -- lives in the tenant database (MecCampus). A user
therefore appears twice under one identity: the control row is what logs in, and
the tenant row is what core.students.user_account_id points at.

Every id is a uuid5 derived from a fixed namespace, so running this twice
produces the same dataset and the SQL can be re-applied without duplicating
anything.
"""

import uuid
import json
import io

NS = uuid.UUID("6f2d1c40-0000-4000-8000-000000000000")
DOMAIN = "mec.local"
PASSWORD = "Mec@2026"


def uid(*parts):
    return str(uuid.uuid5(NS, "|".join(parts)))


def q(value):
    """A SQL literal. None becomes NULL; everything else is quoted."""
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, (dict, list)):
        return "'" + json.dumps(value).replace("'", "''") + "'::jsonb"
    return "'" + str(value).replace("'", "''") + "'"


# ---------------------------------------------------------------- structure --

DEPARTMENTS = [
    ("AIDS", "Artificial Intelligence and Data Science"),
    ("CSBS", "Computer Science and Business Systems"),
    ("IT", "Information Technology"),
    ("CYBER", "Cyber Security"),
    ("CSE", "Computer Science and Engineering"),
    ("AIML", "Artificial Intelligence and Machine Learning"),
]
DEPT_INTAKE = {"AIDS": 33, "CSBS": 34, "IT": 33, "CYBER": 33, "CSE": 34, "AIML": 33}
assert sum(DEPT_INTAKE.values()) == 200

ACADEMIC_YEAR = ("2026-27", "Academic Year 2026-27", "2026-07-01", "2027-05-31")
TERMS = [
    ("ODD", "Odd Semester", 1, "2026-07-01", "2026-11-30"),
    ("EVEN", "Even Semester", 2, "2026-12-15", "2027-05-31"),
]

SUBJECTS_PER_DEPT = [
    ("101", "Data Structures", 4),
    ("102", "Database Management Systems", 4),
    ("103", "Operating Systems", 3),
    ("104", "Computer Networks", 3),
    ("105", "Professional Ethics", 2),
]

# shop_key, display name, category, number of captains
SHOPS = [
    ("mec-canteen", "Campus Canteen", "canteen", 5),
    ("mec-stationery", "Campus Stationery", "stationery", 1),
    ("mec-laundry", "Campus Laundry", "laundry", 1),
]

FIRST_NAMES_M = ["Arun", "Karthik", "Vignesh", "Surya", "Hari", "Dinesh", "Manoj",
                 "Praveen", "Sathish", "Naveen", "Rahul", "Ajith", "Gokul", "Bharath",
                 "Sanjay", "Vimal", "Ashwin", "Deepak", "Ravi", "Nithin"]
FIRST_NAMES_F = ["Divya", "Priya", "Kavya", "Sneha", "Anitha", "Meena", "Lakshmi",
                 "Nandhini", "Swathi", "Ramya", "Keerthi", "Janani", "Abinaya",
                 "Sowmya", "Harini", "Preethi", "Yamuna", "Devi", "Aishwarya",
                 "Vaishnavi"]
LAST_NAMES = ["Kumar", "Raman", "Subramanian", "Krishnan", "Natarajan", "Iyer",
              "Pillai", "Murugan", "Sekar", "Balaji", "Venkatesh", "Rajan",
              "Chandran", "Ganesan", "Sundaram", "Anand"]


def person_name(index, female):
    pool = FIRST_NAMES_F if female else FIRST_NAMES_M
    return "%s %s" % (pool[index % len(pool)], LAST_NAMES[(index // 3) % len(LAST_NAMES)])


def initials(name):
    bits = [p for p in name.split() if p]
    return ((bits[0][0] + (bits[-1][0] if len(bits) > 1 else "")) or "?").upper()


# -------------------------------------------------------------- permissions --

def keys(module, *feature_actions):
    return ["%s.%s" % (module, fa) for fa in feature_actions]


STUDENT_GRANTS = (
    keys("academics", "analysis.read", "assignments.read", "attendance.read",
         "marks.read", "programme.read", "subject.read", "timetable.read", "records.read")
    + keys("attendance", "leave.create", "leave.read", "records.read", "swipe.create",
           "swipe.read")
    + keys("examination", "grades.read", "revaluation.create", "revaluation.read",
           "scheduling.read", "transcript.read", "dashboard.read")
    + keys("timetable", "schedule.read", "publication.read")
    + keys("canteen", "menu.read", "order.create", "order.read", "order.update", "wallet.read")
    + keys("gatepass", "outpass.create", "outpass.read", "leave.create", "leave.read",
           "access.read", "visitor.create", "visitor.read")
    + keys("library", "records.read", "visit_pass.create", "visit_pass.read", "qr_pass.read",
           "occupancy.read", "visit_history.read")
    + keys("fees", "records.read")
    + keys("tuition_fee", "invoice.read", "payment.create", "payment.read")
    + keys("transport", "records.read")
    + keys("placement", "records.read")
    + keys("documents", "records.read")
    + keys("hostel", "records.read")
)

FACULTY_GRANTS = (
    keys("academics", "analysis.read", "assignments.read", "assignments.manage",
         "attendance.read", "marks.read", "programme.read", "subject.read",
         "timetable.read", "records.create", "records.read", "records.update",
         "timetable.substitution.request")
    + keys("attendance", "records.create", "records.read", "records.update",
           "roster.read", "roster.update", "session.create", "swipe.create",
           "swipe.read", "reports.create", "leave.read")
    + keys("examination", "marks.create", "marks.read", "marks.update", "conduct.create",
           "conduct.read", "conduct.update", "grades.read", "scheduling.read",
           "reports.read", "dashboard.read")
    + keys("timetable", "schedule.read", "publication.read", "substitution.create",
           "substitution.read")
    + keys("canteen", "menu.read", "order.create", "order.read", "wallet.read")
    + keys("gatepass", "outpass.create", "outpass.read", "access.read")
    + keys("library", "records.read", "visit_pass.create", "visit_pass.read", "qr_pass.read")
    + keys("documents", "records.read")
    + keys("students", "directory.read")
)

# An advisor is the faculty bundle with more on it, never a separate role, so
# this list is only the difference. Holders carry `staff` as well.
ADVISOR_EXTRA = (
    keys("attendance", "leave.approve", "reports.publish", "session.publish", "records.delete")
    + keys("academics", "records.delete")
    + keys("gatepass", "outpass.approve", "outpass.reject", "leave.approve")
    + keys("examination", "eligibility.read")
    + keys("documents", "records.create", "records.update")
)

HOD_EXTRA = ADVISOR_EXTRA + (
    keys("academics", "programme.create", "programme.update", "subject.create",
         "subject.update", "timetable.manage", "timetable.substitution.approve")
    + keys("examination", "moderation.read", "moderation.update", "moderation.approve",
           "eligibility.approve", "grades.approve", "publishing.read", "publishing.approve",
           "config.read", "degree_audit.read")
    + keys("timetable", "schedule.create", "schedule.update", "schedule.delete",
           "config.read", "publication.approve", "substitution.approve")
    + keys("students", "status.suspend")
    + keys("authorization", "users.read")
    + keys("documents", "records.delete")
)

# An HOD carries the whole faculty bundle again rather than only the difference.
# Scope is merged per permission key at the broadest rank, so a key listed only
# under `staff` would reach no further than the sections that HOD personally
# teaches -- department scope has to be attached to the shared keys too, not
# just to the ones unique to the role.
HOD_GRANTS = sorted(set(FACULTY_GRANTS + HOD_EXTRA))

PRINCIPAL_GRANTS = sorted(set(
    FACULTY_GRANTS + HOD_EXTRA
    + keys("examination", "publishing.publish", "degree_audit.approve", "ai_insights.read",
           "config.create", "config.update", "transcript.create", "transcript.read")
    + keys("timetable", "config.create", "config.update", "publication.publish")
    + keys("attendance", "parent.read")
    + keys("fees", "records.read", "approvals.approve")
    + keys("canteen", "analytics.read")
    + keys("hostel", "records.read")
    + keys("transport", "records.read")
    + keys("library", "records.read")
    + keys("placement", "records.read")
    + keys("students", "directory.read", "directory.create")
    + keys("gatepass", "visitor.read", "visitor.create", "visitor.approve")
    + keys("dashboard", "fee_readiness.read", "follow_ups.read", "pipeline_spread.read",
           "source_quality.read", "track_team.read", "counselor_sla.read")
))

LIBRARIAN_GRANTS = (
    keys("library", "records.create", "records.read", "records.update", "records.delete",
         "occupancy.read", "qr_pass.read", "visit_history.read", "visit_pass.create",
         "visit_pass.read")
    + keys("students", "directory.read")
    + keys("documents", "records.read")
)

SECURITY_GRANTS = (
    keys("gatepass", "access.read", "access.update", "outpass.read", "outpass.verify",
         "outpass.approve", "outpass.reject", "scan.create", "scan.read",
         "visitor.read", "records.create", "records.read", "records.update")
    + keys("students", "directory.read")
)

WARDEN_GRANTS = (
    keys("hostel", "records.create", "records.read", "records.update", "records.delete")
    + keys("gatepass", "outpass.approve", "outpass.reject", "outpass.read",
           "leave.approve", "leave.read", "access.read")
    + keys("attendance", "records.read", "roster.read")
    + keys("students", "directory.read")
    + keys("documents", "records.read")
)

ACCOUNTANT_GRANTS = (
    keys("fees", "records.create", "records.read", "records.update", "records.delete",
         "approvals.approve", "refunds.prepare", "refunds.approve")
    + keys("tuition_fee", "invoice.create", "invoice.read", "payment.create", "payment.read")
    + keys("examination", "eligibility.read")
    + keys("canteen", "analytics.read")
    + keys("vendor_management", "payments.create", "payments.read", "payments.approve",
           "contracts.read", "purchase_orders.read")
    + keys("dashboard", "fee_readiness.read")
    + keys("students", "directory.read")
)

MANAGER_GRANTS = (
    keys("canteen", "menu.create", "menu.read", "menu.update", "menu.delete",
         "order.read", "order.update", "orders.manage", "analytics.read",
         "wallet.read", "wallet.update", "wallet.top_up")
    + keys("vendor_management", "vendors.create", "vendors.read", "vendors.update",
           "vendors.delete", "contracts.create", "contracts.read", "contracts.update",
           "purchase_orders.create", "purchase_orders.read", "purchase_orders.approve",
           "work_orders.create", "work_orders.read", "work_orders.update",
           "payments.create", "payments.read")
    + keys("transport", "records.create", "records.read", "records.update", "records.delete")
    + keys("hostel", "records.create", "records.read", "records.update")
    + keys("library", "records.read")
    + keys("gatepass", "records.read", "access.read", "visitor.read", "visitor.create",
           "visitor.approve")
    + keys("students", "directory.read")
    + keys("documents", "records.create", "records.read", "records.update")
    + keys("dashboard", "fee_readiness.read", "follow_ups.read", "track_team.read")
)

# A shop owner runs one counter: the menu is theirs and so is the till.
SHOP_OWNER_GRANTS = keys(
    "canteen", "menu.create", "menu.read", "menu.update", "menu.delete",
    "order.read", "order.update", "orders.manage", "analytics.read",
    "wallet.read", "wallet.update", "wallet.top_up")

# A captain works the counter but does not set the menu.
SHOP_CAPTAIN_GRANTS = keys(
    "canteen", "menu.read", "order.read", "order.update", "orders.manage", "wallet.read")

# role_key, name, team, portal_family, scope, grants, description
ROLES = [
    ("student", "Student", "Students", "student", "own", STUDENT_GRANTS,
     "Sees their own academic record, orders, passes and fees"),
    ("staff", "Faculty", "Academics", "staff", "assigned", FACULTY_GRANTS,
     "Teaches assigned sections and records their attendance and marks"),
    ("class_advisor", "Class Advisor", "Academics", "staff", "assigned", ADVISOR_EXTRA,
     "Faculty grants plus approval over the section they advise"),
    ("hod", "Head of Department", "Academics", "staff", "department", HOD_GRANTS,
     "Runs one department: its programmes, subjects, timetable and results"),
    ("principal", "Principal", "Administration", "admin", "institution", PRINCIPAL_GRANTS,
     "Institution-wide academic authority, including timetable publication"),
    ("librarian", "Librarian", "Library", "staff", "institution", LIBRARIAN_GRANTS,
     "Runs the library: catalogue, visits and passes"),
    ("security", "Security", "Campus Security", "staff", "institution", SECURITY_GRANTS,
     "Works the gate: verifies passes, logs visitors and scans"),
    ("warden", "Hostel Warden", "Hostel", "staff", "institution", WARDEN_GRANTS,
     "Runs one hostel and approves the outpasses of its residents"),
    ("accountant", "Accountant", "Finance", "admin", "institution", ACCOUNTANT_GRANTS,
     "Fees, refunds, vendor payments and exam eligibility on the finance side"),
    ("manager", "Operations Manager", "Operations", "admin", "institution", MANAGER_GRANTS,
     "Runs campus operations: vendors, transport, hostel and canteen"),
    ("owner", "Vendor Shop Owner", "Vendors", "staff", "institution", SHOP_OWNER_GRANTS,
     "Owns one campus shop and controls its menu, orders and till"),
    ("captain", "Vendor Shop Captain", "Vendors", "staff", "institution", SHOP_CAPTAIN_GRANTS,
     "Works a campus shop counter: takes and fulfils orders"),
    ("superadmin", "Super Administrator", "Administration", "admin", "all", ["*"],
     "Unrestricted access across every module"),
]


# The stored permission_key is authoritative and does not always equal
# "module.feature.action" -- academics.assignments.manage, canteen.orders.manage,
# fees.refunds.prepare and students.status.suspend all break that pattern. The
# grant insert skips keys the tenant does not define, so without this check a
# typo would silently cost a role its permission. permission_keys.txt is dumped
# straight from authz.permission_definitions.
try:
    with io.open("seed/mec/permission_keys.txt", encoding="utf-8") as fh:
        DEFINED = set(line.strip() for line in fh if line.strip())
except IOError:
    DEFINED = set()

if DEFINED:
    requested = set()
    for _, _, _, _, _, grants, _ in ROLES:
        requested.update(grants)
    unknown = sorted(requested - DEFINED - set(["*"]))
    if unknown:
        raise SystemExit(
            "permission keys not defined for this tenant: " + ", ".join(unknown))


# ------------------------------------------------------------------- people --

class Person(object):
    def __init__(self, email, name, roles, account_type, kind, extra=None):
        self.email = "%s@%s" % (email, DOMAIN)
        self.name = name
        self.roles = roles
        self.account_type = account_type
        self.kind = kind
        self.extra = extra or {}
        self.id = uid("user", self.email)
        self.dept = None


people = []
students = []

# Students --------------------------------------------------------------------
# The roll is built in two passes. Deciding residency inside the department
# loop would fill the hostels from the first departments alphabetically and
# leave the rest entirely day scholars; taking every second student of each
# gender instead spreads 50 boys and 50 girls evenly across all six.
roster = []
seq = 0
for dept_code, _ in DEPARTMENTS:
    for _ in range(DEPT_INTAKE[dept_code]):
        seq += 1
        roster.append((seq, dept_code, seq % 2 == 1))

residency = {}
for female in (False, True):
    same_gender = [entry for entry in roster if entry[2] is female]
    bed = 0
    for position, (student_seq, _, _) in enumerate(same_gender):
        if position % 2 == 0:
            bed += 1
            residency[student_seq] = bed

for student_seq, dept_code, female in roster:
    bed = residency.get(student_seq)
    # roll, dept, year and team are read straight off identity.users.profile by
    # the login response -- an empty profile means an empty header in the app.
    profile = {
        "gender": "female" if female else "male",
        "residency": "hosteller" if bed else "day_scholar",
        "dept": dept_code,
        "roll": "MEC26%s%03d" % (dept_code[:2], student_seq),
        "year": "I",
        "team": "Students",
        "section": "A",
    }
    if bed:
        # Four heads to a room, filling rooms from 101 upward.
        profile["hostel"] = "Girls Hostel" if female else "Boys Hostel"
        profile["room"] = "%s-%d" % ("GH" if female else "BH",
                                     100 + ((bed - 1) // 4) + 1)
    person = Person("student%03d" % student_seq, person_name(student_seq, female),
                    ["student"], "student", "student", profile)
    person.dept = dept_code
    person.number = "MEC26%s%03d" % (dept_code[:2], student_seq)
    people.append(person)
    students.append(person)

# Staff -- 20 in total: 1 principal, 6 HODs, 6 advisors, 7 plain faculty -------
staff = []
principal = Person("principal", person_name(3, False), ["principal"], "staff",
                   "employee", {"designation": "Principal"})
people.append(principal)
staff.append(principal)

hods = {}
for i, (dept_code, _) in enumerate(DEPARTMENTS):
    p = Person("hod.%s" % dept_code.lower(), person_name(40 + i, i % 2 == 0),
               ["staff", "hod"], "staff", "employee",
               {"designation": "Head of Department, %s" % dept_code})
    p.dept = dept_code
    people.append(p)
    staff.append(p)
    hods[dept_code] = p

advisors = {}
for i, (dept_code, _) in enumerate(DEPARTMENTS):
    p = Person("advisor.%s" % dept_code.lower(), person_name(70 + i, i % 2 == 1),
               ["staff", "class_advisor"], "staff", "employee",
               {"designation": "Class Advisor, %s" % dept_code})
    p.dept = dept_code
    people.append(p)
    staff.append(p)
    advisors[dept_code] = p

plain_faculty = []
for i in range(7):
    p = Person("faculty%02d" % (i + 1), person_name(100 + i, i % 2 == 0), ["staff"],
               "staff", "employee", {"designation": "Assistant Professor"})
    p.dept = DEPARTMENTS[i % len(DEPARTMENTS)][0]
    people.append(p)
    staff.append(p)
    plain_faculty.append(p)

assert len(staff) == 20, len(staff)

# Support ---------------------------------------------------------------------
support = []
support.append(Person("librarian", person_name(130, True), ["librarian"], "staff",
                      "employee", {"designation": "Librarian"}))
for i in range(3):
    support.append(Person("security%d" % (i + 1), person_name(140 + i, False),
                          ["security"], "staff", "employee",
                          {"designation": "Security Officer", "gate": "Main Gate"}))
for key, label in (("boys", "Boys Hostel"), ("girls", "Girls Hostel")):
    support.append(Person("warden.%s" % key,
                          person_name(150 if key == "boys" else 151, key == "girls"),
                          ["warden"], "staff", "employee",
                          {"designation": "Warden, %s" % label, "hostel": label}))

# Management ------------------------------------------------------------------
management = []
for email, roles, designation in (
    ("admin", ["tenant_admin"], "Tenant Administrator"),
    ("superadmin", ["superadmin"], "Super Administrator"),
    ("accountant", ["accountant"], "Accountant"),
):
    management.append(Person(email, person_name(160 + len(management), False), roles,
                             "staff", "employee", {"designation": designation}))
for i in range(6):
    management.append(Person("manager%d" % (i + 1), person_name(170 + i, i % 2 == 1),
                             ["manager"], "staff", "employee",
                             {"designation": "Operations Manager"}))

people.extend(support)
people.extend(management)
employees = staff + support + management

# Vendors ---------------------------------------------------------------------
vendor_people = []  # (person, shop_key, assignment_role)
for shop_key, shop_name, category, captain_count in SHOPS:
    owner = Person("%s.owner" % category, person_name(200 + len(vendor_people), False),
                   ["owner"], "staff", "vendor", {"shop": shop_name, "post": "owner"})
    people.append(owner)
    vendor_people.append((owner, shop_key, "owner"))
    for c in range(captain_count):
        captain = Person("%s.captain%d" % (category, c + 1),
                         person_name(210 + len(vendor_people), c % 2 == 0),
                         ["captain"], "staff", "vendor",
                         {"shop": shop_name, "post": "captain"})
        people.append(captain)
        vendor_people.append((captain, shop_key, "captain"))

assert len(people) == 245, len(people)

# The login response reads `team` and `dept` off the profile as well, so every
# non-student account gets them from the most specific role it holds -- the last
# entry, since an advisor is ["staff", "class_advisor"] and an HOD is
# ["staff", "hod"].
ROLE_TEAMS = dict((role[0], role[2]) for role in ROLES)
ROLE_TEAMS["tenant_admin"] = "Administration"

for person in people:
    if person.kind == "student":
        continue
    person.extra["team"] = ROLE_TEAMS.get(person.roles[-1], "Staff")
    person.extra["dept"] = person.dept or ""


# ---------------------------------------------------------------------- ids --

TENANT = "(SELECT id FROM platform.tenants WHERE slug = 'mec')"
campus_id = uid("campus", "MEC-MAIN")
year_id = uid("year", ACADEMIC_YEAR[0])
dept_ids = dict((c, uid("dept", c)) for c, _ in DEPARTMENTS)
prog_ids = dict((c, uid("prog", c)) for c, _ in DEPARTMENTS)
batch_ids = dict((c, uid("batch", c)) for c, _ in DEPARTMENTS)
section_ids = dict((c, uid("section", c)) for c, _ in DEPARTMENTS)
shop_ids = dict((k, uid("shop", k)) for k, _, _, _ in SHOPS)

# ------------------------------------------------------------- control plane --

out = io.StringIO()
w = out.write
w("-- Madras Engineering College -- control plane (SuperCampusControl).\n")
w("-- Roles, their grants, the people, and which roles each person holds.\n")
w("-- Generated by seed/mec/generate_seed.py. Re-running is safe.\n")
w("\\set ON_ERROR_STOP on\n")
w("BEGIN;\n\n")
w("-- One bcrypt hash, reused for every account: this is a development dataset\n")
w("-- with a single shared password, so per-account salts would buy nothing and\n")
w("-- cost 245 rounds of key stretching.\n")
w("CREATE TEMP TABLE mec_secret AS SELECT crypt(%s, gen_salt('bf', 12)) AS hash;\n\n"
  % q(PASSWORD))

w("-- Roles ----------------------------------------------------------------------\n")
for role_key, name, team, family, scope, grants, description in ROLES:
    w("""INSERT INTO authz.roles (id, tenant_id, role_key, name, team, scope_description,
        portal_family, protected, active, created_by, updated_by)
VALUES (%s::uuid, %s, %s, %s, %s, %s, %s, false, true, 'mec-seed', 'mec-seed')
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    name = EXCLUDED.name, team = EXCLUDED.team,
    scope_description = EXCLUDED.scope_description,
    portal_family = EXCLUDED.portal_family, active = true,
    updated_by = 'mec-seed', updated_at = now();\n\n"""
      % (q(uid("role", role_key)), TENANT, q(role_key), q(name), q(team),
         q(description), q(family)))

w("""-- Every role reaches both surfaces; what differs is the grants, not the door.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, surface.name, 'mec-seed'
FROM authz.roles role
CROSS JOIN (VALUES ('app'), ('website')) AS surface(name)
WHERE role.tenant_id = %s
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

""" % TENANT)

w("-- Grants ---------------------------------------------------------------------\n")
w("""-- Requested keys are matched against authz.permission_definitions rather than
-- inserted blind, so a key this tenant does not define is skipped instead of
-- breaking the foreign key. The query at the end of this file reports anything
-- dropped that way -- an empty result is the expected outcome.
CREATE TEMP TABLE mec_requested (role_key text, permission_key text, scope text);
""")
for role_key, _, _, _, scope, grants, _ in ROLES:
    for key in sorted(set(grants)):
        w("INSERT INTO mec_requested VALUES (%s, %s, %s);\n"
          % (q(role_key), q(key), q(scope)))

w("""
DELETE FROM authz.role_permissions WHERE tenant_id = %s AND granted_by = 'mec-seed';

INSERT INTO authz.role_permissions
    (tenant_id, role_id, surface, permission_key, scope, granted_by)
SELECT role.tenant_id, role.id, surface.name, request.permission_key, request.scope, 'mec-seed'
FROM mec_requested request
JOIN authz.roles role
    ON role.tenant_id = %s AND role.role_key = request.role_key
JOIN authz.permission_definitions definition
    ON definition.tenant_id = role.tenant_id
   AND definition.permission_key = request.permission_key
CROSS JOIN (VALUES ('app'), ('website')) AS surface(name)
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope, granted_by = 'mec-seed', granted_at = now();

""" % (TENANT, TENANT))

w("-- People ---------------------------------------------------------------------\n")
for p in people:
    w("""INSERT INTO identity.users (id, email, password_hash, display_name, initials,
        account_type, active, profile)
VALUES (%s::uuid, %s, (SELECT hash FROM mec_secret), %s, %s, %s, true, %s)
ON CONFLICT (email) DO UPDATE SET
    display_name = EXCLUDED.display_name, initials = EXCLUDED.initials,
    account_type = EXCLUDED.account_type, active = true,
    profile = EXCLUDED.profile, updated_at = now();\n"""
      % (q(p.id), q(p.email), q(p.name), q(initials(p.name)), q(p.account_type),
         q(p.extra)))

w("\n-- Memberships ----------------------------------------------------------------\n")
for p in people:
    roles_array = "ARRAY[" + ",".join(q(r) for r in p.roles) + "]::text[]"
    w("""INSERT INTO identity.tenant_memberships (tenant_id, user_id, roles, active,
        is_primary, profile)
VALUES (%s, %s::uuid, %s, true, true, %s)
ON CONFLICT (tenant_id, user_id) DO UPDATE SET
    roles = EXCLUDED.roles, active = true, is_primary = true,
    profile = EXCLUDED.profile, updated_at = now();\n"""
      % (TENANT, q(p.id), roles_array, q(p.extra)))

w("""
-- authz.user_roles mirrors the membership array; the admin console reads it.
DELETE FROM authz.user_roles WHERE tenant_id = %s AND assigned_by = 'mec-seed';
INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
SELECT membership.tenant_id, membership.user_id, role.id, 'mec-seed'
FROM identity.tenant_memberships membership
JOIN authz.roles role
    ON role.tenant_id = membership.tenant_id
   AND role.role_key = ANY(membership.roles)
WHERE membership.tenant_id = %s
ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;

COMMIT;

SELECT request.role_key, request.permission_key AS unmatched_permission
FROM mec_requested request
LEFT JOIN authz.permission_definitions definition
    ON definition.tenant_id = %s AND definition.permission_key = request.permission_key
WHERE definition.permission_key IS NULL
ORDER BY 1, 2;
""" % (TENANT, TENANT, TENANT))

with io.open("seed/mec/01_control.sql", "w", encoding="utf-8", newline="\n") as fh:
    fh.write(out.getvalue())

# -------------------------------------------------------------- tenant plane --

out = io.StringIO()
w = out.write
w("-- Madras Engineering College -- campus data (MecCampus).\n")
w("-- The academic structure, the roll, the staff and the three campus shops.\n")
w("-- Generated by seed/mec/generate_seed.py. Re-running is safe.\n")
w("\\set ON_ERROR_STOP on\n")
w("BEGIN;\n\n")

w("-- Campus, year and terms ------------------------------------------------------\n")
w("""INSERT INTO core.campuses (id, tenant_id, code, name, active)
VALUES (%s::uuid, %s, 'MEC-MAIN', 'Madras Engineering College - Main Campus', true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

INSERT INTO core.academic_years (id, tenant_id, code, name, starts_on, ends_on, status)
VALUES (%s::uuid, %s, %s, %s, %s::date, %s::date, 'active')
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, status = 'active';

""" % (q(campus_id), TENANT, q(year_id), TENANT, q(ACADEMIC_YEAR[0]), q(ACADEMIC_YEAR[1]),
       q(ACADEMIC_YEAR[2]), q(ACADEMIC_YEAR[3])))

for code, name, seqno, start, end in TERMS:
    w("""INSERT INTO core.terms (id, tenant_id, academic_year_id, code, name, sequence,
        starts_on, ends_on, status)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, %d, %s::date, %s::date, 'active')
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, status = 'active';\n"""
      % (q(uid("term", code)), TENANT, q(year_id), q(code), q(name), seqno,
         q(start), q(end)))

w("\n-- Departments, programmes, batches, sections ----------------------------------\n")
for code, name in DEPARTMENTS:
    w("""INSERT INTO core.departments (id, tenant_id, campus_id, code, name, active)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

INSERT INTO core.programmes (id, tenant_id, department_id, code, name, duration_terms, active)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, 8, true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

INSERT INTO core.batches (id, tenant_id, programme_id, academic_year_id, code, name,
        starts_on, ends_on, active)
VALUES (%s::uuid, %s, %s::uuid, %s::uuid, %s, %s, '2026-07-01'::date, '2030-05-31'::date, true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

INSERT INTO core.sections (id, tenant_id, batch_id, code, name, capacity, active)
VALUES (%s::uuid, %s, %s::uuid, 'A', %s, 60, true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

""" % (q(dept_ids[code]), TENANT, q(campus_id), q(code), q(name),
       q(prog_ids[code]), TENANT, q(dept_ids[code]), q("BE-" + code), q("B.E. " + name),
       q(batch_ids[code]), TENANT, q(prog_ids[code]), q(year_id), q(code + "-2026"),
       q(code + " Batch of 2026-2030"),
       q(section_ids[code]), TENANT, q(batch_ids[code]), q(code + " - Section A")))

w("-- Subjects and offerings -------------------------------------------------------\n")
for code, _ in DEPARTMENTS:
    for suffix, sname, credits in SUBJECTS_PER_DEPT:
        w("""INSERT INTO core.subjects (id, tenant_id, department_id, code, name, credits, active)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, %d, true)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, active = true;

INSERT INTO core.subject_offerings (id, tenant_id, subject_id, academic_year_id, term_id,
        section_id, active)
VALUES (%s::uuid, %s, %s::uuid, %s::uuid, %s::uuid, %s::uuid, true)
ON CONFLICT (id) DO UPDATE SET active = true;

""" % (q(uid("subject", code, suffix)), TENANT, q(dept_ids[code]), q(code + suffix),
       q(sname), credits,
       q(uid("offering", code, suffix)), TENANT, q(uid("subject", code, suffix)),
       q(year_id), q(uid("term", "ODD")), q(section_ids[code])))

w("""-- Identity mirror ---------------------------------------------------------------
-- core.students.user_account_id and core.employees.user_id point at this
-- database's identity.users, not the control plane's. The ids match the control
-- rows exactly, so the two sides describe one person. No password lives here --
-- authentication only ever reads the control plane.
""")
for p in people:
    w("""INSERT INTO identity.users (id, email, password_hash, display_name, initials,
        account_type, active, profile)
VALUES (%s::uuid, %s, 'seeded-in-control-plane', %s, %s, %s, true, %s)
ON CONFLICT (email) DO UPDATE SET
    display_name = EXCLUDED.display_name, initials = EXCLUDED.initials,
    account_type = EXCLUDED.account_type, active = true, profile = EXCLUDED.profile;\n"""
      % (q(p.id), q(p.email), q(p.name), q(initials(p.name)), q(p.account_type), q(p.extra)))

w("\n-- The roll --------------------------------------------------------------------\n")
for p in students:
    w("""INSERT INTO core.students (id, tenant_id, student_number, full_name, email,
        applicant_id, application_id, admission_id, campus_id, department_id, program_id,
        batch_id, section_id, academic_year, admission_category, user_account_id, status, profile)
VALUES (%s::uuid, %s, %s, %s, %s, %s::uuid, %s::uuid, %s::uuid, %s::uuid, %s::uuid,
        %s::uuid, %s::uuid, %s::uuid, %s, %s, %s::uuid, 'active', %s)
ON CONFLICT (id) DO UPDATE SET
    full_name = EXCLUDED.full_name, section_id = EXCLUDED.section_id,
    department_id = EXCLUDED.department_id, status = 'active',
    profile = EXCLUDED.profile, updated_at = now();\n"""
      % (q(uid("student", p.email)), TENANT, q(p.number), q(p.name), q(p.email),
         q(uid("applicant", p.email)), q(uid("application", p.email)),
         q(uid("admission", p.email)), q(campus_id), q(dept_ids[p.dept]),
         q(prog_ids[p.dept]), q(batch_ids[p.dept]), q(section_ids[p.dept]),
         q(ACADEMIC_YEAR[0]), q(p.extra["residency"]), q(p.id), q(p.extra)))

w("""
-- Enrolments --------------------------------------------------------------------
-- core.students.section_id is what the attendance roster reads, but the
-- timetable and academic-assignment queries resolve a student's section through
-- core.academic_enrollments instead. Both have to exist, or a student sees a
-- roster and an empty timetable.
""")
for p in students:
    w("""INSERT INTO core.academic_enrollments (id, tenant_id, student_id, academic_year_id,
        term_id, campus_id, department_id, programme_id, batch_id, section_id, status, started_at)
VALUES (%s::uuid, %s, %s::uuid, %s::uuid, %s::uuid, %s::uuid, %s::uuid, %s::uuid, %s::uuid,
        %s::uuid, 'active', '2026-07-01T00:00:00Z'::timestamptz)
ON CONFLICT (id) DO UPDATE SET
    section_id = EXCLUDED.section_id, status = 'active', updated_at = now();\n"""
      % (q(uid("enrollment", p.email)), TENANT, q(uid("student", p.email)), q(year_id),
         q(uid("term", "ODD")), q(campus_id), q(dept_ids[p.dept]), q(prog_ids[p.dept]),
         q(batch_ids[p.dept]), q(section_ids[p.dept])))

w("""
-- Guardians ---------------------------------------------------------------------
-- The outpass chain starts with a parent, and a parent has no account: they are
-- reached on WhatsApp and answer through a one-time link. So a guardian is a
-- record with a phone number, not a login.
--
-- The numbers below are in the reserved +91 90000 block and are deliberately
-- unreachable. Point one at a real handset to test delivery:
--
--   UPDATE core.guardians SET phone = '+91XXXXXXXXXX'
--    WHERE student_id = (SELECT id FROM core.students WHERE student_number = 'MEC26AI001');
""")
GUARDIAN_RELATION = ("Father", "Mother")
for index, learner in enumerate(students):
    relation = GUARDIAN_RELATION[index % 2]
    surname = learner.name.split()[-1]
    guardian_name = "%s %s" % (
        person_name(300 + index, relation == "Mother").split()[0], surname)
    w("""INSERT INTO core.guardians (id, tenant_id, student_id, full_name, phone, email,
        relationship, is_primary)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, %s, %s, true)
ON CONFLICT (id) DO UPDATE SET
    full_name = EXCLUDED.full_name, phone = EXCLUDED.phone,
    relationship = EXCLUDED.relationship, is_primary = true, updated_at = now();\n"""
      % (q(uid("guardian", learner.email)), TENANT, q(uid("student", learner.email)),
         q(guardian_name), q("+9190000%05d" % (index + 1)),
         q("guardian.%s" % learner.email), q(relation)))

w("\n-- Employees -------------------------------------------------------------------\n")
for i, p in enumerate(employees, start=1):
    dept = "%s::uuid" % q(dept_ids[p.dept]) if p.dept else "NULL"
    w("""INSERT INTO core.employees (id, tenant_id, user_id, employee_number, department_id,
        full_name, email, status, profile)
VALUES (%s::uuid, %s, %s::uuid, %s, %s, %s, %s, 'active', %s)
ON CONFLICT (id) DO UPDATE SET
    full_name = EXCLUDED.full_name, department_id = EXCLUDED.department_id,
    status = 'active', profile = EXCLUDED.profile, updated_at = now();\n"""
      % (q(uid("employee", p.email)), TENANT, q(p.id), q("MECEMP%03d" % i), dept,
         q(p.name), q(p.email), q(p.extra)))

w("""
-- Teaching assignments -----------------------------------------------------------
-- This is what makes `assigned` scope mean anything. A faculty member reaches a
-- section because they teach an offering in it, through
-- teaching_assignments -> subject_offerings.section_id. Without a row here a
-- section-scoped grant resolves to nothing.
""")
assign_index = 0
for code, _ in DEPARTMENTS:
    for n, (suffix, _, _) in enumerate(SUBJECTS_PER_DEPT):
        if n == 0:
            teacher = advisors[code]
        elif n == 1:
            teacher = hods[code]
        else:
            teacher = plain_faculty[assign_index % len(plain_faculty)]
            assign_index += 1
        w("""INSERT INTO core.teaching_assignments (id, tenant_id, subject_offering_id,
        faculty_user_id, assignment_type, active, assigned_by)
VALUES (%s::uuid, %s, %s::uuid, %s::uuid, 'primary', true, %s::uuid)
ON CONFLICT (id) DO UPDATE SET faculty_user_id = EXCLUDED.faculty_user_id, active = true;\n"""
          % (q(uid("teaching", code, suffix)), TENANT, q(uid("offering", code, suffix)),
             q(teacher.id), q(principal.id)))

w("""
-- A fortnight of attendance ------------------------------------------------------
-- Without published sessions every student reads 0% and every dashboard reads
-- "no attendance recorded yet", which is true but shows nothing. Ten working
-- days of the first subject per section gives the roll something real to report.
-- The status has to be `published_to_hod`: that, and submitted_to_principal, are
-- what the summary query counts. A draft session contributes nothing.
-- Absences are spread deterministically rather than randomly so the seed stays
-- reproducible from one run to the next.
""")
ATTENDANCE_DAYS = ["2026-08-10", "2026-08-11", "2026-08-12", "2026-08-13",
                   "2026-08-14", "2026-08-17", "2026-08-18", "2026-08-19",
                   "2026-08-20", "2026-08-21"]

students_by_dept = {}
for learner in students:
    students_by_dept.setdefault(learner.dept, []).append(learner)

for dept_code, _ in DEPARTMENTS:
    suffix, subject_name, _credits = SUBJECTS_PER_DEPT[0]
    teacher = advisors[dept_code]
    for day_index, held_on in enumerate(ATTENDANCE_DAYS):
        session_id = uid("session", dept_code, held_on)
        w("""INSERT INTO campus_ops.attendance_sessions (id, tenant_id, subject_offering_id,
        section_id, subject_name, faculty_user_id, held_on, period_label, status)
VALUES (%s::uuid, %s, %s::uuid, %s::uuid, %s, %s::uuid, %s::date, %s, 'published_to_hod')
ON CONFLICT (id) DO UPDATE SET status = 'published_to_hod';\n"""
          % (q(session_id), TENANT, q(uid("offering", dept_code, suffix)),
             q(section_ids[dept_code]), q(subject_name), q(teacher.id),
             q(held_on), q("Period 1")))
        for seat, learner in enumerate(students_by_dept[dept_code]):
            # A spread rather than a switch: most of the roll lands in the
            # eighties and nineties, and every eleventh student is a frequent
            # absentee who falls under the seventy-five percent line, so the
            # shortfall colouring has something to colour.
            frequent = seat % 11 == 0
            absent = (
                day_index % 2 == 0
                if frequent
                else (seat * 3 + day_index) % 8 == 0
            )
            w("""INSERT INTO campus_ops.attendance_entries (tenant_id, session_id,
        student_user_id, student_name, status, marked_by)
VALUES (%s, %s::uuid, %s::uuid, %s, %s, %s::uuid)
ON CONFLICT (tenant_id, session_id, student_user_id) DO UPDATE SET status = EXCLUDED.status;\n"""
              % (TENANT, q(session_id), q(learner.id), q(learner.name),
                 q("absent" if absent else "present"), q(teacher.id)))
        w("\n")

w("\n-- Campus shops ----------------------------------------------------------------\n")
for shop_key, shop_name, category, _ in SHOPS:
    w("""INSERT INTO campus_ops.shops (id, tenant_id, shop_key, name, category, description,
        is_active, meal_compliance, qr_payments, created_by)
VALUES (%s::uuid, %s, %s, %s, %s, %s, true, %s, true, 'mec-seed')
ON CONFLICT (id) DO UPDATE SET
    name = EXCLUDED.name, category = EXCLUDED.category, is_active = true, updated_at = now();\n"""
      % (q(shop_ids[shop_key]), TENANT, q(shop_key), q(shop_name), q(category),
         q(shop_name + " at Madras Engineering College"), q(category == "canteen")))

w("\n")
for person, shop_key, assignment_role in vendor_people:
    w("""INSERT INTO campus_ops.shop_user_assignments (tenant_id, shop_id, user_id,
        assignment_role, is_active, assigned_by)
VALUES (%s, %s::uuid, %s::uuid, %s, true, 'mec-seed')
ON CONFLICT (tenant_id, shop_id, user_id) DO UPDATE SET
    assignment_role = EXCLUDED.assignment_role, is_active = true, updated_at = now();\n"""
      % (TENANT, q(shop_ids[shop_key]), q(person.id), q(assignment_role)))

w("\nCOMMIT;\n")

with io.open("seed/mec/02_campus.sql", "w", encoding="utf-8", newline="\n") as fh:
    fh.write(out.getvalue())

# ------------------------------------------------------------- credentials --
# Written from the same objects the SQL is written from, so the account list
# cannot drift from the dataset it documents.

doc = io.StringIO()
d = doc.write
d("# Madras Engineering College - accounts\n\n")
d("Generated by `seed/mec/generate_seed.py`. Every account below shares the\n")
d("password `%s`, and every one is a local development account on the\n" % PASSWORD)
d("`mec` tenant.\n\n")
d("| count | who | email | roles |\n|---|---|---|---|\n")

rows = [
    (len(students), "Students",
     "student001@%s ... student%03d@%s" % (DOMAIN, len(students), DOMAIN), "student"),
    (1, "Principal", "principal@%s" % DOMAIN, "principal"),
    (len(hods), "Heads of Department",
     ", ".join("hod.%s@%s" % (c.lower(), DOMAIN) for c, _ in DEPARTMENTS), "staff + hod"),
    (len(advisors), "Class Advisors",
     ", ".join("advisor.%s@%s" % (c.lower(), DOMAIN) for c, _ in DEPARTMENTS),
     "staff + class_advisor"),
    (len(plain_faculty), "Faculty",
     "faculty01@%s ... faculty%02d@%s" % (DOMAIN, len(plain_faculty), DOMAIN), "staff"),
    (1, "Librarian", "librarian@%s" % DOMAIN, "librarian"),
    (3, "Security", "security1@%s, security2@%s, security3@%s" % (DOMAIN, DOMAIN, DOMAIN),
     "security"),
    (2, "Wardens", "warden.boys@%s, warden.girls@%s" % (DOMAIN, DOMAIN), "warden"),
    (1, "Tenant Admin", "admin@%s" % DOMAIN, "tenant_admin"),
    (1, "Super Admin", "superadmin@%s" % DOMAIN, "superadmin"),
    (6, "Managers", "manager1@%s ... manager6@%s" % (DOMAIN, DOMAIN), "manager"),
    (1, "Accountant", "accountant@%s" % DOMAIN, "accountant"),
]
for shop_key, shop_name, category, captain_count in SHOPS:
    rows.append((1, "%s owner" % shop_name, "%s.owner@%s" % (category, DOMAIN), "owner"))
    tail = (" ... %s.captain%d@%s" % (category, captain_count, DOMAIN)
            if captain_count > 1 else "")
    rows.append((captain_count, "%s captains" % shop_name,
                 "%s.captain1@%s%s" % (category, DOMAIN, tail), "captain"))
for count, who, email, roles in rows:
    d("| %d | %s | `%s` | `%s` |\n" % (count, who, email, roles))
d("\n**Total: %d accounts.**\n\n" % len(people))

d("## Scope ladder\n\n")
d("Scope is what separates these personas; not one of them is a hard-coded role.\n\n")
d("| role | scope | reach |\n|---|---|---|\n")
for role_key, name, _, _, scope, grants, description in ROLES:
    d("| `%s` | `%s` | %s |\n" % (role_key, scope, description))
d("\n`assigned` is the wire value the app reads as `PermissionScope.section`.\n")

with io.open("seed/mec/CREDENTIALS.md", "w", encoding="utf-8", newline="\n") as fh:
    fh.write(doc.getvalue())

hostellers = sum(1 for s in students if s.extra["residency"] == "hosteller")
print("people          : %d" % len(people))
print("  students      : %d (%d hostellers, %d day scholars)"
      % (len(students), hostellers, len(students) - hostellers))
print("  staff         : %d (1 principal, %d HODs, %d advisors, %d faculty)"
      % (len(staff), len(hods), len(advisors), len(plain_faculty)))
print("  support       : %d" % len(support))
print("  management    : %d" % len(management))
print("  vendor people : %d across %d shops" % (len(vendor_people), len(SHOPS)))
print("roles           : %d (+ tenant_admin from bootstrap)" % len(ROLES))
print("employees       : %d" % len(employees))
print("wrote 01_control.sql, 02_campus.sql and CREDENTIALS.md into seed/mec/")
