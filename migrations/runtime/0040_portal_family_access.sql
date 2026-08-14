-- A role describes permissions inside one fixed portal shell. The shell is
-- explicit so tenant-defined role names never control application routing.
ALTER TABLE authz.roles
    ADD COLUMN IF NOT EXISTS portal_family text NOT NULL DEFAULT 'staff';

ALTER TABLE authz.roles
    DROP CONSTRAINT IF EXISTS roles_portal_family_check;

ALTER TABLE authz.roles
    ADD CONSTRAINT roles_portal_family_check
    CHECK (portal_family IN ('student', 'parent', 'staff', 'admin'));

UPDATE authz.roles
SET portal_family = CASE
    WHEN role_key = 'student' THEN 'student'
    WHEN role_key IN ('parent', 'guardian') THEN 'parent'
    WHEN role_key IN ('tenant_admin', 'admin', 'administrator', 'super_admin') THEN 'admin'
    ELSE 'staff'
END;

CREATE INDEX IF NOT EXISTS roles_tenant_portal_family_idx
    ON authz.roles (tenant_id, portal_family)
    WHERE active;

ALTER TABLE authz.role_permissions
    DROP CONSTRAINT IF EXISTS role_permissions_scope_check;
ALTER TABLE authz.role_permissions
    ADD CONSTRAINT role_permissions_scope_check
    CHECK (scope IN ('own', 'assigned', 'department', 'institution', 'all'));

ALTER TABLE authz.assignments
    DROP CONSTRAINT IF EXISTS assignments_scope_check;
ALTER TABLE authz.assignments
    ADD CONSTRAINT assignments_scope_check
    CHECK (scope IN ('own', 'assigned', 'department', 'institution', 'all'));
