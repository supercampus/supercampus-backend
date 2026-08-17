INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES
    ('students.directory.read', 'students', 'directory', 'read', ARRAY['read']::text[], 'View student directory', 'View tenant Student Master records', true),
    ('students.directory.create', 'students', 'directory', 'create', ARRAY['create']::text[], 'Import students', 'Create and bulk import tenant Student Master records', true)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
CROSS JOIN authz.permission_templates template
WHERE template.permission_key IN ('students.directory.read', 'students.directory.create')
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, granted_at)
SELECT role.tenant_id, role.id, permission.permission_key, 'all', '{}'::jsonb, role.created_by, now()
FROM authz.roles role
CROSS JOIN (VALUES ('students.directory.read'), ('students.directory.create')) permission(permission_key)
WHERE role.role_key = 'tenant_admin'
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;
