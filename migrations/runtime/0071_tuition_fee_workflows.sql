-- Complete tenant-configurable Tuition Fee workflow permission catalog.
-- Access remains permission-driven; these definitions are available to every
-- tenant and can be assigned independently on website and app surfaces.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT format('tuition_fee.%s.%s', workflow.key, action.key),
       'tuition_fee', workflow.key, action.key, ARRAY[action.crud]::text[],
       format('%s %s', action.label, workflow.label),
       format('%s records in the %s tuition fee workflow', action.label, workflow.label),
       true
FROM (VALUES
    ('fee_heads', 'Fee Heads'),
    ('fee_structures', 'Fee Structures'),
    ('fee_assignment', 'Fee Assignment'),
    ('installment_plans', 'Installment Plans'),
    ('student_fee_accounts', 'Student Fee Accounts'),
    ('payments', 'Payments'),
    ('fines_penalties', 'Fines & Penalties'),
    ('reconciliation', 'Reconciliation'),
    ('fee_defaulters', 'Fee Defaulters'),
    ('fee_notifications', 'Fee Notifications'),
    ('fee_reports', 'Fee Reports')
) AS workflow(key, label)
CROSS JOIN (VALUES
    ('create', 'create', 'Create'),
    ('read', 'read', 'Read'),
    ('update', 'update', 'Update'),
    ('delete', 'delete', 'Archive')
) AS action(key, crud, label)
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
SELECT tenant.id, template.permission_key, template.module_key,
       template.feature_key, template.action, template.crud_actions,
       template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.module_key = 'tuition_fee'
 AND template.permission_key LIKE 'tuition_fee.%'
WHERE template.active
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- Existing accountant roles receive the management workspace on both
-- surfaces. Tenant administrators already inherit `*`; every other role is
-- intentionally unchanged and can be configured through Users & Roles.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'institution',
       '{}'::jsonb, 'runtime-migration-0071', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id = role.tenant_id
 AND permission.module_key = 'tuition_fee'
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key = 'accountant'
  AND role.active
  AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = now();
