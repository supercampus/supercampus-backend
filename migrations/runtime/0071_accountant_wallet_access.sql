-- Accountants credit the tenant wallet used by every campus shop. Grant both
-- delivery surfaces so the role behaves consistently on web and mobile.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'institution',
       '{}'::jsonb, 'wallet-accountant-migration', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id=role.tenant_id
 AND permission.permission_key IN ('canteen.wallet.read','canteen.wallet.top_up')
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key='accountant' AND role.active AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope=EXCLUDED.scope,
    constraints=EXCLUDED.constraints,
    granted_by=EXCLUDED.granted_by,
    granted_at=now();
