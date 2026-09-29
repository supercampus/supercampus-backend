-- Permissions for ad-hoc payment requests and online payment tracking.
--
-- Tenant admins already hold '*'. The accounts office (accountant role) gets
-- all four grants on both surfaces. The API applies this file at startup
-- against the authorization database (payment_requests::ensure_permissions).
-- Safe to run more than once.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT permission_key, module_key, feature_key, action, crud_actions, display_name, description, true
FROM (VALUES
    ('fees.payment_requests.read', 'fees', 'payment_requests', 'read', ARRAY['read']::text[],
     'View payment requests', 'View ad-hoc payment requests and who has paid'),
    ('fees.payment_requests.manage', 'fees', 'payment_requests', 'manage',
     ARRAY['create','update']::text[], 'Manage payment requests',
     'Create and cancel payment requests and mark students as paid'),
    ('fees.online_payments.read', 'fees', 'online_payments', 'read', ARRAY['read']::text[],
     'View online payments', 'Track Razorpay capture, settlement and recovery'),
    ('fees.online_payments.reconcile', 'fees', 'online_payments', 'reconcile',
     ARRAY['update']::text[], 'Reconcile online payments',
     'Sync payment status and settlements from Razorpay and recover captured payments')
) AS permission(permission_key, module_key, feature_key, action, crud_actions, display_name, description)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key IN (
    'fees.payment_requests.read', 'fees.payment_requests.manage',
    'fees.online_payments.read', 'fees.online_payments.reconcile'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'institution',
       '{}'::jsonb, 'payment-requests-migration', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id = role.tenant_id
 AND permission.permission_key IN (
    'fees.payment_requests.read', 'fees.payment_requests.manage',
    'fees.online_payments.read', 'fees.online_payments.reconcile'
 )
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key = 'accountant' AND role.active AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO NOTHING;
