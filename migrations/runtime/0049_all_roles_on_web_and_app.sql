-- Every tenant role is available on both clients. Permissions remain scoped
-- independently by surface, but role creation itself is never surface-limited.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, available.surface, 'surface-backfill'
FROM authz.roles role
CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;
