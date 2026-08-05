INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
VALUES
    ('crm.leads.import', 'crm', 'leads', 'import', 'Import leads',
     'Bulk import validated CRM leads with tenant-scoped duplicate handling')
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
JOIN authz.permission_templates AS template
  ON template.permission_key = 'crm.leads.import'
ON CONFLICT (tenant_id, permission_key) DO UPDATE
SET module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();
