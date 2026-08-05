-- Dynamic permissions for generic platform configuration and module records.
-- Typed module APIs (for example CRM leads) continue to use their more
-- specific permission keys.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT permission_key, module_key, feature_key, action, ARRAY[action]::text[],
       display_name, description, true
FROM (VALUES
    ('platform.configuration.read', 'platform', 'configuration', 'read',
     'View runtime configuration', 'Read versioned institution configuration documents'),
    ('platform.configuration.update', 'platform', 'configuration', 'update',
     'Update runtime configuration', 'Create new versions of institution configuration documents')
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

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT module_key || '.records.' || action,
       module_key,
       'records',
       action,
       ARRAY[action]::text[],
       initcap(action) || ' ' || initcap(replace(module_key, '_', ' ')) || ' records',
       initcap(action) || ' dynamic records in the ' || module_key || ' module',
       true
FROM unnest(ARRAY[
    'crm', 'admissions', 'academics', 'attendance', 'documents', 'examinations',
    'fees', 'gatepass', 'hostel', 'library', 'placement', 'transport'
]) AS module(module_key)
CROSS JOIN unnest(ARRAY['create', 'read', 'update', 'delete']) AS operation(action)
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
       template.action, template.crud_actions, template.display_name,
       template.description, true
FROM platform.tenants AS tenant
CROSS JOIN authz.permission_templates AS template
WHERE template.permission_key IN (
    'platform.configuration.read', 'platform.configuration.update'
)
OR template.permission_key ~ '^(crm|admissions|academics|attendance|documents|examinations|fees|gatepass|hostel|library|placement|transport)\.records\.(create|read|update|delete)$'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();
