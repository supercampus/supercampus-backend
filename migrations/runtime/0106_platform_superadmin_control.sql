-- SuperCampus internal control plane. This role is intentionally separate from
-- tenant administrators and is the only role allowed to read cross-tenant data.
ALTER TABLE authz.roles
    DROP CONSTRAINT IF EXISTS roles_portal_family_check;

ALTER TABLE authz.roles
    ADD CONSTRAINT roles_portal_family_check
    CHECK (portal_family IN ('student', 'parent', 'staff', 'admin', 'platform-control'));

INSERT INTO platform.tenants (slug, code, name, city, status)
VALUES ('supercampus-control', 'SC-CONTROL', 'SuperCampus Operations', 'Platform', 'active')
ON CONFLICT (slug) DO UPDATE SET
    code = EXCLUDED.code,
    name = EXCLUDED.name,
    city = EXCLUDED.city,
    status = 'active',
    updated_at = now();

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
VALUES
    ('platform.control.*', 'platform', 'control', 'manage',
     'Manage the SuperCampus control plane',
     'Cross-tenant operations, health, support, billing and audit access')
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name, description)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.display_name, template.description
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key = 'platform.control.*'
WHERE tenant.slug = 'supercampus-control'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    active = true,
    updated_at = now();

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, active, created_by, updated_by)
SELECT id, 'platform_super_admin', 'Platform Super Admin', 'SuperCampus',
       'Controls all tenants and platform operations', 'platform-control',
       true, true, 'runtime-migration-0106', 'runtime-migration-0106'
FROM platform.tenants
WHERE slug = 'supercampus-control'
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    name = EXCLUDED.name,
    team = EXCLUDED.team,
    scope_description = EXCLUDED.scope_description,
    portal_family = EXCLUDED.portal_family,
    protected = true,
    active = true,
    updated_at = now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, granted_by)
SELECT role.tenant_id, role.id, definition.permission_key, 'all', 'runtime-migration-0106'
FROM authz.roles role
JOIN authz.permission_definitions definition
  ON definition.tenant_id = role.tenant_id
 AND definition.permission_key = 'platform.control.*'
WHERE role.role_key = 'platform_super_admin'
ON CONFLICT (tenant_id, role_id, permission_key) DO UPDATE SET
    scope = 'all',
    granted_by = EXCLUDED.granted_by,
    granted_at = now();

CREATE TABLE IF NOT EXISTS platform.support_tickets (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid REFERENCES platform.tenants(id) ON DELETE SET NULL,
    subject text NOT NULL,
    description text NOT NULL DEFAULT '',
    requester_name text NOT NULL DEFAULT '',
    requester_email text NOT NULL DEFAULT '',
    priority text NOT NULL DEFAULT 'normal'
        CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    status text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'waiting', 'resolved', 'closed')),
    assigned_to uuid REFERENCES identity.users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

CREATE INDEX IF NOT EXISTS support_tickets_status_idx
    ON platform.support_tickets (status, priority, created_at DESC);
CREATE INDEX IF NOT EXISTS support_tickets_tenant_idx
    ON platform.support_tickets (tenant_id, created_at DESC);
