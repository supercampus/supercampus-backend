-- One permission definition must represent exactly one CRUD action. Sharing a
-- permission key across Create and Update made the admin matrix toggle both.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT permission_key, module_key, feature_key, action, ARRAY[action]::text[], display_name, description, true
FROM (VALUES
    ('authorization.roles.create', 'authorization', 'roles', 'create', 'Create roles', 'Create tenant roles'),
    ('authorization.roles.update', 'authorization', 'roles', 'update', 'Update roles', 'Update roles and replace permission grants'),
    ('authorization.roles.delete', 'authorization', 'roles', 'delete', 'Delete roles', 'Delete non-protected tenant roles'),
    ('authorization.users.create', 'authorization', 'users', 'create', 'Create users', 'Create or join tenant users'),
    ('authorization.users.update', 'authorization', 'users', 'update', 'Update users', 'Assign tenant roles to users'),
    ('crm.forms.create', 'crm', 'forms', 'create', 'Create forms', 'Create CRM form definitions'),
    ('crm.forms.update', 'crm', 'forms', 'update', 'Update forms', 'Update CRM form definitions'),
    ('crm.forms.delete', 'crm', 'forms', 'delete', 'Delete forms', 'Delete CRM form definitions'),
    ('crm.templates.create', 'crm', 'templates', 'create', 'Create templates', 'Create communication templates'),
    ('crm.templates.update', 'crm', 'templates', 'update', 'Update templates', 'Update communication templates'),
    ('crm.assignment.create', 'crm', 'assignment', 'create', 'Create assignment capacity', 'Create counselor capacity and routing'),
    ('crm.assignment.update', 'crm', 'assignment', 'update', 'Update assignment capacity', 'Update counselor capacity and routing'),
    ('crm.configuration.create', 'crm', 'configuration', 'create', 'Create CRM configuration', 'Create workflow and automation configuration'),
    ('crm.configuration.update', 'crm', 'configuration', 'update', 'Update CRM configuration', 'Update workflow and automation configuration'),
    ('crm.campaigns.create', 'crm', 'campaigns', 'create', 'Create campaigns', 'Create campaign performance records'),
    ('crm.campaigns.update', 'crm', 'campaigns', 'update', 'Update campaigns', 'Update campaign performance records')
) AS permission(permission_key, module_key, feature_key, action, display_name, description)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants AS tenant
CROSS JOIN authz.permission_templates AS template
WHERE template.permission_key IN (
    'authorization.roles.create', 'authorization.roles.update', 'authorization.roles.delete',
    'authorization.users.create', 'authorization.users.update',
    'crm.forms.create', 'crm.forms.update', 'crm.forms.delete',
    'crm.templates.create', 'crm.templates.update',
    'crm.assignment.create', 'crm.assignment.update',
    'crm.configuration.create', 'crm.configuration.update',
    'crm.campaigns.create', 'crm.campaigns.update'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- Preserve the authority of existing roles while replacing shared grants with
-- independent action grants.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, granted_at)
SELECT existing_grant.tenant_id, existing_grant.role_id, mapping.new_key, existing_grant.scope,
       existing_grant.constraints, existing_grant.granted_by, existing_grant.granted_at
FROM authz.role_permissions AS existing_grant
JOIN (VALUES
    ('authorization.roles.manage', 'authorization.roles.create'),
    ('authorization.roles.manage', 'authorization.roles.update'),
    ('authorization.roles.manage', 'authorization.roles.delete'),
    ('authorization.users.manage', 'authorization.users.create'),
    ('authorization.users.manage', 'authorization.users.update'),
    ('crm.forms.manage', 'crm.forms.create'),
    ('crm.forms.manage', 'crm.forms.update'),
    ('crm.forms.manage', 'crm.forms.delete'),
    ('crm.templates.manage', 'crm.templates.create'),
    ('crm.templates.manage', 'crm.templates.update'),
    ('crm.assignment.manage', 'crm.assignment.create'),
    ('crm.assignment.manage', 'crm.assignment.update'),
    ('crm.configuration.manage', 'crm.configuration.create'),
    ('crm.configuration.manage', 'crm.configuration.update'),
    ('crm.campaigns.manage', 'crm.campaigns.create'),
    ('crm.campaigns.manage', 'crm.campaigns.update')
) AS mapping(old_key, new_key) ON mapping.old_key = existing_grant.permission_key
ON CONFLICT (tenant_id, role_id, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by;

DELETE FROM authz.role_permissions
WHERE permission_key IN (
    'authorization.roles.manage', 'authorization.users.manage', 'crm.forms.manage',
    'crm.templates.manage', 'crm.assignment.manage', 'crm.configuration.manage',
    'crm.campaigns.manage'
);

UPDATE authz.permission_templates
SET active = false, crud_actions = '{}'::text[], updated_at = now()
WHERE permission_key IN (
    'authorization.roles.manage', 'authorization.users.manage', 'crm.forms.manage',
    'crm.templates.manage', 'crm.assignment.manage', 'crm.configuration.manage',
    'crm.campaigns.manage'
);

UPDATE authz.permission_definitions
SET active = false, crud_actions = '{}'::text[], updated_at = now()
WHERE permission_key IN (
    'authorization.roles.manage', 'authorization.users.manage', 'crm.forms.manage',
    'crm.templates.manage', 'crm.assignment.manage', 'crm.configuration.manage',
    'crm.campaigns.manage'
);
