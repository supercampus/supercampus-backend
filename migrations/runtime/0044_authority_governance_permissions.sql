-- Sensitive authority capabilities. These definitions intentionally create no
-- role grants: tenant role setup remains explicit and backend policy checks are
-- still required at each protected command endpoint.

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES
    ('fees.approvals.approve', 'fees', 'approvals', 'approve', ARRAY['update']::text[], 'Approve fee decision', 'Approve fee structures, concessions and adjustments other than refunds', true),
    ('fees.refunds.prepare', 'fees', 'refunds', 'create', ARRAY['create']::text[], 'Prepare refund request', 'Prepare a refund request for Management approval', true),
    ('fees.refunds.approve', 'fees', 'refunds', 'approve', ARRAY['update']::text[], 'Approve refund', 'Issue final Management approval for a refund', true),
    ('students.status.suspend', 'students', 'status', 'approve', ARRAY['update']::text[], 'Approve student suspension', 'Approve a student suspension with reason and audit history', true),
    ('academics.assignments.manage', 'academics', 'assignments', 'update', ARRAY['update']::text[], 'Manage academic assignments', 'Assign departments to HODs and classes or subjects to Faculty', true)
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
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key IN (
      'fees.approvals.approve',
      'fees.refunds.prepare',
      'fees.refunds.approve',
      'students.status.suspend',
      'academics.assignments.manage'
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
