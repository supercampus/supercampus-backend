CREATE TABLE IF NOT EXISTS campus_ops.wallet_top_up_settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    minimum_amount numeric(14,2) NOT NULL DEFAULT 50 CHECK (minimum_amount >= 1),
    maximum_amount numeric(14,2) NOT NULL DEFAULT 5000 CHECK (maximum_amount >= minimum_amount),
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO campus_ops.wallet_top_up_settings (tenant_id)
SELECT id FROM platform.tenants
ON CONFLICT (tenant_id) DO NOTHING;

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
VALUES
    ('canteen.wallet.configure', 'canteen', 'wallet', 'configure',
     'Configure wallet top-up limits',
     'Set the minimum and maximum amount students can add through online checkout')
ON CONFLICT (permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,
    feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,
    display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,
    active=true,
    updated_at=now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name, description)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.display_name, template.description
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key='canteen.wallet.configure'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,
    feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,
    display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,
    active=true,
    updated_at=now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'institution',
       '{}'::jsonb, 'wallet-settings-migration', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id=role.tenant_id
 AND permission.permission_key='canteen.wallet.configure'
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key='accountant' AND role.active AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope=EXCLUDED.scope,
    constraints=EXCLUDED.constraints,
    granted_by=EXCLUDED.granted_by,
    granted_at=now();
