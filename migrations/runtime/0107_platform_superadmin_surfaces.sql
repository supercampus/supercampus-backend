-- Ensure platform_super_admin role is enabled on web and app surfaces
-- and granted platform.control.* permission.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, available.surface, 'runtime-migration-0107'
FROM authz.roles role
JOIN platform.tenants tenant ON tenant.id = role.tenant_id
CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
WHERE role.role_key = 'platform_super_admin'
  AND tenant.slug = 'supercampus-control'
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, granted_by, surface)
SELECT role.tenant_id, role.id, 'platform.control.*', 'all', 'runtime-migration-0107', available.surface
FROM authz.roles role
JOIN platform.tenants tenant ON tenant.id = role.tenant_id
CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
WHERE role.role_key = 'platform_super_admin'
  AND tenant.slug = 'supercampus-control'
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = 'all',
    granted_by = EXCLUDED.granted_by;
