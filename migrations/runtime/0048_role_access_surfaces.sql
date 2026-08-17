-- Portal family describes the experience (student, parent, staff, admin).
-- Surface describes where that experience is available (website or app).
-- They are deliberately independent.

ALTER TABLE authz.role_permissions
    DROP CONSTRAINT IF EXISTS role_permissions_pkey;

ALTER TABLE authz.role_permissions
    ADD COLUMN IF NOT EXISTS surface text NOT NULL DEFAULT 'website';

ALTER TABLE authz.role_permissions
    DROP CONSTRAINT IF EXISTS role_permissions_surface_check;

ALTER TABLE authz.role_permissions
    ADD CONSTRAINT role_permissions_surface_check
    CHECK (surface IN ('app', 'website'));

ALTER TABLE authz.role_permissions
    ADD CONSTRAINT role_permissions_pkey
    PRIMARY KEY (tenant_id, role_id, surface, permission_key);

-- Role grants were historically effective on both clients. Preserve that exact
-- behavior while making subsequent edits surface-specific.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, granted_at, surface)
SELECT tenant_id, role_id, permission_key, scope, constraints, granted_by, granted_at, 'app'
FROM authz.role_permissions
WHERE surface = 'website'
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO NOTHING;

CREATE TABLE IF NOT EXISTS authz.role_surfaces (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    role_id uuid NOT NULL REFERENCES authz.roles(id) ON DELETE CASCADE,
    surface text NOT NULL CHECK (surface IN ('app', 'website')),
    enabled_by text NOT NULL,
    enabled_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, role_id, surface)
);

-- Existing roles were previously usable from either client.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT tenant_id, id, surface, updated_by
FROM authz.roles
CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

CREATE INDEX IF NOT EXISTS role_surfaces_role_idx
    ON authz.role_surfaces (tenant_id, role_id, surface);

ALTER TABLE authz.role_surfaces ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON authz.role_surfaces;
CREATE POLICY tenant_isolation ON authz.role_surfaces
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
