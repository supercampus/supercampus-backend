-- Direct user access can either add grants or deny permissions inherited from
-- roles. Deny wins during effective access resolution, which lets an admin
-- disable one module for one user without editing the shared role.

ALTER TABLE authz.assignments
    ADD COLUMN IF NOT EXISTS mode text NOT NULL DEFAULT 'allow';

ALTER TABLE authz.assignments
    DROP CONSTRAINT IF EXISTS assignments_mode_check;
ALTER TABLE authz.assignments
    ADD CONSTRAINT assignments_mode_check CHECK (mode IN ('allow', 'deny'));

CREATE INDEX IF NOT EXISTS assignments_effective_mode_idx
    ON authz.assignments (tenant_id, principal_id, surface, mode, active);
