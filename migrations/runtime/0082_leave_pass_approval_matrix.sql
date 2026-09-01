-- Leave Pass is a college-hours gate workflow for every student.
-- Outpass remains a separate hostel-only parent/warden workflow.

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, created_by, updated_by)
SELECT tenant.id, 'principal', 'Principal', 'Administration',
       'Final approval for college-hours leave passes', 'staff', false,
       'runtime-migration-0082', 'runtime-migration-0082'
FROM platform.tenants tenant
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    name=EXCLUDED.name, team=EXCLUDED.team,
    scope_description=EXCLUDED.scope_description,
    portal_family='staff', active=true, updated_at=now(),
    updated_by='runtime-migration-0082';

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, created_by, updated_by)
SELECT tenant.id, 'hod', 'Head of Department', 'Academics',
       'Department-level leave-pass approval', 'staff', false,
       'runtime-migration-0082', 'runtime-migration-0082'
FROM platform.tenants tenant
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    name=EXCLUDED.name, team=EXCLUDED.team,
    scope_description=EXCLUDED.scope_description,
    portal_family='staff', active=true, updated_at=now(),
    updated_by='runtime-migration-0082';

INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, surface.name, 'runtime-migration-0082'
FROM authz.roles role
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
WHERE role.role_key IN ('student','class_advisor','hod','principal')
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by, granted_at)
SELECT role.tenant_id, role.id, grant_row.permission_key, surface.name,
       grant_row.scope, '{}'::jsonb, 'runtime-migration-0082', now()
FROM authz.roles role
JOIN (VALUES
    ('student', 'gatepass.leave.create', 'own'),
    ('student', 'gatepass.leave.read', 'own'),
    ('class_advisor', 'gatepass.leave.read', 'department'),
    ('class_advisor', 'gatepass.leave.approve', 'department'),
    ('hod', 'gatepass.leave.read', 'department'),
    ('hod', 'gatepass.leave.approve', 'department'),
    ('principal', 'gatepass.leave.read', 'institution'),
    ('principal', 'gatepass.leave.approve', 'institution')
) grant_row(role_key, permission_key, scope)
  ON grant_row.role_key=role.role_key
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
JOIN authz.permission_definitions definition
  ON definition.tenant_id=role.tenant_id
 AND definition.permission_key=grant_row.permission_key
 AND definition.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope=EXCLUDED.scope, constraints=EXCLUDED.constraints,
    granted_by=EXCLUDED.granted_by, granted_at=EXCLUDED.granted_at;
