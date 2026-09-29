-- Permanent user deletion from the Admin Console. Deleting is separate from
-- deactivating (authorization.users.update): it removes the membership and
-- sign-in for good, so it has its own grant.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES
    ('authorization.users.delete', 'authorization', 'users', 'delete', ARRAY['delete']::text[],
     'Delete users',
     'Permanently remove a user''s membership, sign-in and personal contact details; history is kept',
     true)
ON CONFLICT (permission_key) DO UPDATE SET active = true, updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key, template.action,
       template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key = 'authorization.users.delete'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET active = true, updated_at = now();

-- Tenant administrators already hold '*'; the explicit grant keeps the key
-- visible on the role. DO NOTHING so an administrator who later removes it
-- from a role is not overridden.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by, granted_at)
SELECT role.tenant_id, role.id, 'authorization.users.delete', surface.name, 'institution', '{}'::jsonb,
       'runtime-migration-0147', now()
FROM authz.roles role
CROSS JOIN (VALUES ('app'::text), ('website'::text)) surface(name)
WHERE role.role_key IN ('tenant_admin', 'admin', 'administrator', 'super_admin') AND role.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO NOTHING;
