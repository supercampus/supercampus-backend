ALTER TABLE authz.permission_templates
    ADD COLUMN IF NOT EXISTS crud_actions text[] NOT NULL DEFAULT '{}'::text[];

ALTER TABLE authz.permission_definitions
    ADD COLUMN IF NOT EXISTS crud_actions text[] NOT NULL DEFAULT '{}'::text[];

UPDATE authz.permission_templates AS template
SET crud_actions = metadata.crud_actions,
    updated_at = now()
FROM (VALUES
    ('*', ARRAY['create', 'read', 'update', 'delete']::text[]),
    ('authorization.permissions.read', ARRAY['read']::text[]),
    ('authorization.permissions.manage', ARRAY['update']::text[]),
    ('authorization.roles.read', ARRAY['read']::text[]),
    ('authorization.roles.manage', ARRAY['create', 'update', 'delete']::text[]),
    ('authorization.users.read', ARRAY['read']::text[]),
    ('authorization.users.manage', ARRAY['create', 'update']::text[]),
    ('crm.leads.read', ARRAY['read']::text[]),
    ('crm.leads.create', ARRAY['create']::text[]),
    ('crm.leads.import', ARRAY['create']::text[]),
    ('crm.leads.update', ARRAY['update']::text[]),
    ('crm.leads.delete', ARRAY['delete']::text[]),
    ('crm.leads.assign', ARRAY['update']::text[]),
    ('crm.leads.claim', ARRAY['create']::text[]),
    ('crm.leads.stage.move', ARRAY['update']::text[]),
    ('crm.leads.hold', ARRAY['update']::text[]),
    ('crm.leads.hold.release', ARRAY['update']::text[]),
    ('crm.leads.archive', ARRAY['delete']::text[]),
    ('crm.leads.unarchive', ARRAY['update']::text[]),
    ('crm.forms.read', ARRAY['read']::text[]),
    ('crm.forms.manage', ARRAY['create', 'update', 'delete']::text[]),
    ('crm.forms.publish', ARRAY['update']::text[]),
    ('crm.forms.submit', ARRAY['create']::text[]),
    ('crm.forms.submissions.read', ARRAY['read']::text[]),
    ('crm.communications.send', ARRAY['create']::text[]),
    ('crm.templates.read', ARRAY['read']::text[]),
    ('crm.templates.manage', ARRAY['create', 'update']::text[]),
    ('crm.assignment.read', ARRAY['read']::text[]),
    ('crm.assignment.manage', ARRAY['create', 'update']::text[]),
    ('crm.configuration.read', ARRAY['read']::text[]),
    ('crm.configuration.manage', ARRAY['create', 'update']::text[]),
    ('crm.dashboard.read', ARRAY['read']::text[]),
    ('crm.erp.handoff', ARRAY['create']::text[]),
    ('crm.reports.read', ARRAY['read']::text[]),
    ('crm.campaigns.read', ARRAY['read']::text[]),
    ('crm.campaigns.manage', ARRAY['create', 'update']::text[])
) AS metadata(permission_key, crud_actions)
WHERE template.permission_key = metadata.permission_key;

UPDATE authz.permission_definitions AS definition
SET crud_actions = template.crud_actions,
    updated_at = now()
FROM authz.permission_templates AS template
WHERE template.permission_key = definition.permission_key;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'permission_templates_crud_actions_check'
          AND conrelid = 'authz.permission_templates'::regclass
    ) THEN
        ALTER TABLE authz.permission_templates
            ADD CONSTRAINT permission_templates_crud_actions_check
            CHECK (crud_actions <@ ARRAY['create', 'read', 'update', 'delete']::text[]);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'permission_definitions_crud_actions_check'
          AND conrelid = 'authz.permission_definitions'::regclass
    ) THEN
        ALTER TABLE authz.permission_definitions
            ADD CONSTRAINT permission_definitions_crud_actions_check
            CHECK (crud_actions <@ ARRAY['create', 'read', 'update', 'delete']::text[]);
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION authz.bootstrap_tenant_permissions()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO authz.permission_definitions
        (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
         display_name, description)
    SELECT NEW.id, template.permission_key, template.module_key, template.feature_key,
           template.action, template.crud_actions, template.display_name,
           template.description
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
