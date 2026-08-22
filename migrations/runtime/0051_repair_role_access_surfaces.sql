-- Roles created by older deployments could be left on only one client surface.
-- Keep every role available to both clients; permission grants remain explicit.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, available.surface, 'surface-repair-0051'
FROM authz.roles role
CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;
