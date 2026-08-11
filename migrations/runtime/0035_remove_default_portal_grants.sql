-- Portal access must be explicitly assigned by the tenant administrator.
-- Migration 0032 seeded broad role grants for demonstration purposes; those
-- grants bypass the admin access-control workflow and must not remain active.
-- Keep grants created later by an administrator or another workflow intact.

DELETE FROM authz.role_permissions AS role_permission
USING authz.roles AS role
WHERE role.id = role_permission.role_id
  AND role.tenant_id = role_permission.tenant_id
  AND role.role_key IN ('student', 'parent', 'warden', 'security')
  AND role_permission.granted_by = 'runtime-migration-0032';
