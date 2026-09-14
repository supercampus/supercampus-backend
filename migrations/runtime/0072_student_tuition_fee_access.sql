-- Students may read only financial records carrying their own stable student
-- identifier. The API enforces this `own` scope for every returned record.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'own',
       '{}'::jsonb, 'runtime-migration-0072', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id = role.tenant_id
 AND permission.permission_key IN (
    'tuition_fee.fee_assignment.read',
    'tuition_fee.student_fee_accounts.read',
    'tuition_fee.payments.read',
    'tuition_fee.fines_penalties.read',
    'tuition_fee.fee_notifications.read'
 )
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key = 'student'
  AND role.active
  AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = 'own',
    constraints = '{}'::jsonb,
    granted_by = 'runtime-migration-0072',
    granted_at = now();
