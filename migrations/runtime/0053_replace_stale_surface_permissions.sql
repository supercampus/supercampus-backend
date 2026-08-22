-- Re-run exact surface mirroring for installations that briefly applied the
-- union-based version of migration 0052 during development.
CREATE TEMP TABLE access_surface_source_0053 ON COMMIT DROP AS
SELECT
    tenant_id,
    role_id,
    CASE
        WHEN bool_or(surface = 'website') THEN 'website'::text
        ELSE 'app'::text
    END AS source_surface
FROM authz.role_permissions
GROUP BY tenant_id, role_id;

CREATE TEMP TABLE authoritative_role_permissions_0053 ON COMMIT DROP AS
SELECT permission.*
FROM authz.role_permissions permission
JOIN access_surface_source_0053 source
  ON source.tenant_id = permission.tenant_id
 AND source.role_id = permission.role_id
 AND source.source_surface = permission.surface;

DELETE FROM authz.role_permissions permission
USING access_surface_source_0053 source
WHERE source.tenant_id = permission.tenant_id
  AND source.role_id = permission.role_id
  AND permission.surface <> source.source_surface;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, granted_at, surface)
SELECT
    permission.tenant_id,
    permission.role_id,
    permission.permission_key,
    permission.scope,
    permission.constraints,
    permission.granted_by,
    permission.granted_at,
    CASE permission.surface WHEN 'website' THEN 'app' ELSE 'website' END
FROM authoritative_role_permissions_0053 permission
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE
SET scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = EXCLUDED.granted_at;
