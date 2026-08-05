CREATE SCHEMA IF NOT EXISTS authz;

CREATE TABLE IF NOT EXISTS authz.permission_templates (
    permission_key text PRIMARY KEY,
    module_key text NOT NULL,
    feature_key text NOT NULL,
    action text NOT NULL,
    display_name text NOT NULL,
    description text NOT NULL DEFAULT '',
    active boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS authz.permission_definitions (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    module_key text NOT NULL,
    feature_key text NOT NULL,
    action text NOT NULL,
    display_name text NOT NULL,
    description text NOT NULL DEFAULT '',
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, permission_key)
);

CREATE TABLE IF NOT EXISTS authz.roles (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    role_key text NOT NULL,
    name text NOT NULL,
    team text NOT NULL DEFAULT 'Custom',
    scope_description text NOT NULL DEFAULT '',
    protected boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_by text NOT NULL,
    updated_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, role_key)
);

CREATE TABLE IF NOT EXISTS authz.role_permissions (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    role_id uuid NOT NULL REFERENCES authz.roles(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    scope text NOT NULL DEFAULT 'all',
    constraints jsonb NOT NULL DEFAULT '{}'::jsonb,
    granted_by text NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, role_id, permission_key),
    FOREIGN KEY (tenant_id, permission_key)
        REFERENCES authz.permission_definitions(tenant_id, permission_key)
        ON DELETE CASCADE,
    CHECK (scope IN ('all', 'assigned', 'own'))
);

CREATE TABLE IF NOT EXISTS authz.user_roles (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE CASCADE,
    role_id uuid NOT NULL REFERENCES authz.roles(id) ON DELETE CASCADE,
    assigned_by text NOT NULL,
    assigned_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id, role_id)
);

CREATE INDEX IF NOT EXISTS authorization_roles_tenant_idx
    ON authz.roles (tenant_id, active, name);
CREATE INDEX IF NOT EXISTS authorization_user_roles_user_idx
    ON authz.user_roles (tenant_id, user_id);
CREATE INDEX IF NOT EXISTS authorization_role_permissions_role_idx
    ON authz.role_permissions (tenant_id, role_id);

ALTER TABLE authz.permission_definitions ENABLE ROW LEVEL SECURITY;
ALTER TABLE authz.roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE authz.role_permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE authz.user_roles ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON authz.permission_definitions;
CREATE POLICY tenant_isolation ON authz.permission_definitions
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
DROP POLICY IF EXISTS tenant_isolation ON authz.roles;
CREATE POLICY tenant_isolation ON authz.roles
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
DROP POLICY IF EXISTS tenant_isolation ON authz.role_permissions;
CREATE POLICY tenant_isolation ON authz.role_permissions
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
DROP POLICY IF EXISTS tenant_isolation ON authz.user_roles;
CREATE POLICY tenant_isolation ON authz.user_roles
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
SELECT permission.permission_key, permission.module_key, permission.feature_key,
       permission.action, permission.display_name, permission.description
FROM (VALUES
    ('*', 'platform', 'tenant', 'manage', 'Full tenant administration', 'All current and future tenant permissions'),
    ('authorization.permissions.read', 'authorization', 'permissions', 'read', 'View permission catalog', 'View tenant permission definitions'),
    ('authorization.permissions.manage', 'authorization', 'permissions', 'manage', 'Manage permission catalog', 'Create and update tenant permission definitions'),
    ('authorization.roles.read', 'authorization', 'roles', 'read', 'View roles', 'View tenant roles and grants'),
    ('authorization.roles.manage', 'authorization', 'roles', 'manage', 'Manage roles', 'Create roles and assign permissions'),
    ('authorization.users.read', 'authorization', 'users', 'read', 'View users', 'View tenant users and role assignments'),
    ('authorization.users.manage', 'authorization', 'users', 'manage', 'Manage users', 'Create tenant users and assign roles'),
    ('crm.leads.read', 'crm', 'leads', 'read', 'View leads', 'Read CRM leads within the granted data scope'),
    ('crm.leads.create', 'crm', 'leads', 'create', 'Create leads', 'Create CRM leads'),
    ('crm.leads.update', 'crm', 'leads', 'update', 'Update leads', 'Update CRM leads within the granted data scope'),
    ('crm.leads.delete', 'crm', 'leads', 'delete', 'Delete leads', 'Soft-delete CRM leads'),
    ('crm.leads.assign', 'crm', 'assignment', 'update', 'Assign leads', 'Assign or reassign leads to any tenant user'),
    ('crm.leads.claim', 'crm', 'assignment', 'create', 'Claim unassigned leads', 'Assign an unassigned lead to the current user'),
    ('crm.leads.stage.move', 'crm', 'pipeline', 'update', 'Move lead stage', 'Move leads through configured pipeline stages'),
    ('crm.leads.hold', 'crm', 'status', 'update', 'Place leads on hold', 'Pause lead progression'),
    ('crm.leads.hold.release', 'crm', 'status', 'update', 'Release lead hold', 'Resume a held lead'),
    ('crm.leads.archive', 'crm', 'status', 'delete', 'Archive leads', 'Archive CRM leads'),
    ('crm.leads.unarchive', 'crm', 'status', 'update', 'Restore archived leads', 'Restore archived CRM leads'),
    ('crm.forms.read', 'crm', 'forms', 'read', 'View forms', 'View CRM form definitions'),
    ('crm.forms.manage', 'crm', 'forms', 'update', 'Manage forms', 'Create, update and delete CRM form definitions'),
    ('crm.forms.publish', 'crm', 'forms', 'publish', 'Publish forms', 'Publish or unpublish CRM forms'),
    ('crm.forms.submit', 'crm', 'forms', 'create', 'Submit forms', 'Submit internal CRM forms'),
    ('crm.forms.submissions.read', 'crm', 'forms', 'read_submissions', 'View form submissions', 'View CRM form submissions'),
    ('crm.communications.send', 'crm', 'communications', 'create', 'Send communications', 'Send CRM communications'),
    ('crm.templates.read', 'crm', 'templates', 'read', 'View templates', 'View communication templates'),
    ('crm.templates.manage', 'crm', 'templates', 'update', 'Manage templates', 'Create and update communication templates'),
    ('crm.assignment.read', 'crm', 'assignment', 'read', 'View assignment capacity', 'View counselor capacity and workload'),
    ('crm.assignment.manage', 'crm', 'assignment', 'manage', 'Manage assignments', 'Configure counselor capacity and routing'),
    ('crm.configuration.read', 'crm', 'configuration', 'read', 'View CRM configuration', 'View tenant CRM workflow configuration'),
    ('crm.configuration.manage', 'crm', 'configuration', 'manage', 'Manage CRM configuration', 'Update tenant CRM workflow configuration'),
    ('crm.dashboard.read', 'crm', 'dashboard', 'read', 'View CRM dashboard', 'View CRM dashboards and operational aggregates'),
    ('crm.erp.handoff', 'crm', 'erp_handoff', 'create', 'Trigger ERP handoff', 'Create a student ERP handoff'),
    ('crm.reports.read', 'crm', 'reports', 'read', 'View CRM reports', 'View CRM reports and exports'),
    ('crm.campaigns.read', 'crm', 'campaigns', 'read', 'View campaigns', 'View campaign performance'),
    ('crm.campaigns.manage', 'crm', 'campaigns', 'manage', 'Manage campaigns', 'Create and update campaign performance')
) AS permission(permission_key, module_key, feature_key, action, display_name, description)
ON CONFLICT (permission_key) DO UPDATE
SET module_key = EXCLUDED.module_key,
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
FROM platform.tenants AS tenant
CROSS JOIN authz.permission_templates AS template
WHERE template.active
ON CONFLICT (tenant_id, permission_key) DO UPDATE
SET module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

CREATE OR REPLACE FUNCTION authz.bootstrap_tenant_permissions()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO authz.permission_definitions
        (tenant_id, permission_key, module_key, feature_key, action, display_name, description)
    SELECT NEW.id, template.permission_key, template.module_key, template.feature_key,
           template.action, template.display_name, template.description
    FROM authz.permission_templates AS template
    WHERE template.active
    ON CONFLICT (tenant_id, permission_key) DO NOTHING;

    INSERT INTO authz.roles
        (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
    VALUES (NEW.id, 'tenant_admin', 'Tenant Admin', 'Administration',
            'Controls tenant users, roles, permissions, settings, and modules', true,
            'tenant-bootstrap', 'tenant-bootstrap')
    ON CONFLICT (tenant_id, role_key) DO NOTHING;

    INSERT INTO authz.role_permissions
        (tenant_id, role_id, permission_key, scope, granted_by)
    SELECT NEW.id, role.id, '*', 'all', 'tenant-bootstrap'
    FROM authz.roles AS role
    WHERE role.tenant_id = NEW.id AND role.role_key = 'tenant_admin'
    ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS bootstrap_tenant_authorization ON platform.tenants;
CREATE TRIGGER bootstrap_tenant_authorization
AFTER INSERT ON platform.tenants
FOR EACH ROW EXECUTE FUNCTION authz.bootstrap_tenant_permissions();

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
SELECT id, 'tenant_admin', 'Tenant Admin', 'Administration',
       'Controls tenant users, roles, permissions, settings, and modules', true,
       'runtime-migration-0009', 'runtime-migration-0009'
FROM platform.tenants
ON CONFLICT (tenant_id, role_key) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, granted_by)
SELECT role.tenant_id, role.id, '*', 'all', 'runtime-migration-0009'
FROM authz.roles AS role
WHERE role.role_key = 'tenant_admin'
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;

-- Preserve existing membership role names as tenant-owned role records without
-- granting any implicit CRM authority. Tenant admins explicitly manifest access.
INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
SELECT DISTINCT membership.tenant_id, legacy.role_key,
       initcap(replace(legacy.role_key, '_', ' ')), 'Imported',
       'Imported from the legacy tenant membership', false,
       'runtime-migration-0009', 'runtime-migration-0009'
FROM identity.tenant_memberships AS membership
CROSS JOIN LATERAL unnest(membership.roles) AS legacy(role_key)
WHERE legacy.role_key <> 'tenant_admin'
ON CONFLICT (tenant_id, role_key) DO NOTHING;

INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
SELECT membership.tenant_id, membership.user_id, role.id, 'runtime-migration-0009'
FROM identity.tenant_memberships AS membership
JOIN authz.roles AS role ON role.tenant_id = membership.tenant_id
WHERE role.role_key = ANY(membership.roles)
ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;
