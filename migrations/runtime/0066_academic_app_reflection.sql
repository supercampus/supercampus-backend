-- Published timetable and attendance access for the academic app surface.
-- Scope is enforced again by the API: students only see their own section,
-- faculty only see assigned classes, and class advisors see their departments.

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, created_by, updated_by)
SELECT tenant.id, 'class_advisor', 'Class advisor', 'Academics',
       'Advises assigned departments and classes', 'staff', false,
       'runtime-migration-0066', 'runtime-migration-0066'
FROM platform.tenants tenant
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    portal_family = 'staff', active = true, updated_at = now(),
    updated_by = 'runtime-migration-0066';

INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, surface.name, 'runtime-migration-0066'
FROM authz.roles role
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
WHERE role.role_key IN ('student', 'staff', 'class_advisor')
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by, granted_at)
SELECT role.tenant_id, role.id, grant_row.permission_key, surface.name,
       grant_row.scope, '{}'::jsonb, 'runtime-migration-0066', now()
FROM authz.roles role
JOIN (VALUES
    ('student', 'academics.timetable.read', 'own'),
    ('student', 'attendance.records.read', 'own'),
    ('staff', 'academics.timetable.read', 'assigned'),
    ('staff', 'attendance.roster.read', 'assigned'),
    ('staff', 'attendance.roster.update', 'assigned'),
    ('staff', 'attendance.session.create', 'assigned'),
    ('staff', 'attendance.session.publish', 'assigned'),
    ('class_advisor', 'academics.timetable.read', 'department'),
    ('class_advisor', 'attendance.roster.read', 'department'),
    ('class_advisor', 'attendance.roster.update', 'department'),
    ('class_advisor', 'attendance.session.create', 'department'),
    ('class_advisor', 'attendance.session.publish', 'department')
) grant_row(role_key, permission_key, scope)
  ON grant_row.role_key = role.role_key
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
JOIN authz.permission_definitions definition
  ON definition.tenant_id = role.tenant_id
 AND definition.permission_key = grant_row.permission_key
 AND definition.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = EXCLUDED.granted_at;

ALTER TABLE campus_ops.attendance_sessions
    ADD COLUMN IF NOT EXISTS timetable_entry_id uuid;

CREATE INDEX IF NOT EXISTS attendance_sessions_timetable_entry_idx
    ON campus_ops.attendance_sessions (tenant_id, timetable_entry_id, held_on);
