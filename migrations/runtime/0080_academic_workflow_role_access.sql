-- Make the governed attendance and marks workflows visible on both supported
-- clients. API assignment checks still narrow records to assigned classes,
-- departments, or the institution after this coarse role grant succeeds.

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, created_by, updated_by)
SELECT tenant.id, preset.role_key, preset.name, 'Academics',
       preset.scope_description, 'staff', false,
       'runtime-migration-0080', 'runtime-migration-0080'
FROM platform.tenants tenant
CROSS JOIN (VALUES
    ('staff', 'Subject staff', 'Takes assigned-class attendance and submits marks'),
    ('class_advisor', 'Class advisor', 'Reviews assigned-class attendance and marks'),
    ('hod', 'Head of department', 'Reviews department attendance and marks'),
    ('principal', 'Principal', 'Reviews institution attendance and marks')
) preset(role_key, name, scope_description)
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    portal_family = 'staff', active = true, updated_at = now(),
    updated_by = 'runtime-migration-0080';

INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, surface.name, 'runtime-migration-0080'
FROM authz.roles role
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
WHERE role.role_key IN ('staff', 'class_advisor', 'hod', 'principal')
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints,
     granted_by, granted_at)
SELECT role.tenant_id, role.id, grant_row.permission_key, surface.name,
       grant_row.scope, '{}'::jsonb, 'runtime-migration-0080', now()
FROM authz.roles role
JOIN (VALUES
    ('staff', 'attendance.roster.read', 'assigned'),
    ('staff', 'attendance.roster.update', 'assigned'),
    ('staff', 'attendance.session.create', 'assigned'),
    ('staff', 'attendance.session.publish', 'assigned'),
    ('staff', 'attendance.records.read', 'assigned'),
    ('staff', 'examination.marks.create', 'assigned'),
    ('staff', 'examination.marks.read', 'assigned'),
    ('staff', 'examination.marks.update', 'assigned'),
    ('class_advisor', 'attendance.roster.read', 'department'),
    ('class_advisor', 'attendance.roster.update', 'department'),
    ('class_advisor', 'attendance.session.create', 'department'),
    ('class_advisor', 'attendance.session.publish', 'department'),
    ('class_advisor', 'attendance.records.read', 'department'),
    ('class_advisor', 'attendance.reports.create', 'department'),
    ('class_advisor', 'attendance.reports.publish', 'department'),
    ('class_advisor', 'examination.marks.create', 'department'),
    ('class_advisor', 'examination.marks.read', 'department'),
    ('class_advisor', 'examination.marks.update', 'department'),
    ('hod', 'attendance.roster.read', 'department'),
    ('hod', 'attendance.records.read', 'department'),
    ('hod', 'attendance.session.publish', 'department'),
    ('hod', 'attendance.reports.create', 'department'),
    ('hod', 'attendance.reports.publish', 'department'),
    ('hod', 'examination.marks.read', 'department'),
    ('hod', 'examination.marks.update', 'department'),
    ('principal', 'attendance.roster.read', 'institution'),
    ('principal', 'attendance.records.read', 'institution'),
    ('principal', 'attendance.session.publish', 'institution'),
    ('principal', 'attendance.reports.create', 'institution'),
    ('principal', 'attendance.reports.publish', 'institution'),
    ('principal', 'examination.marks.read', 'institution'),
    ('principal', 'examination.marks.update', 'institution')
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
