-- Backfill the Library lending grants on both client surfaces. Migration 0093
-- was initially released without an explicit surface and therefore inherited
-- the legacy `website` default, leaving Flutter app sessions unauthorized.

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by, granted_at)
SELECT role.tenant_id, role.id, grant_row.permission_key, surface.name,
       grant_row.scope, '{}'::jsonb, 'runtime-migration-0094', now()
FROM authz.roles role
JOIN (VALUES
    ('student', 'library.catalog.read', 'institution'),
    ('student', 'library.loan.create', 'own'),
    ('student', 'library.loan.read', 'own'),
    ('student', 'library.favourite.update', 'own'),
    ('librarian', 'library.catalog.read', 'institution'),
    ('librarian', 'library.catalog.manage', 'institution'),
    ('librarian', 'library.loan.read', 'institution'),
    ('librarian', 'library.loan.approve', 'institution'),
    ('librarian', 'library.settings.update', 'institution')
) AS grant_row(role_key, permission_key, scope)
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
