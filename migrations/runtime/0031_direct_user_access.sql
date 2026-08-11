-- Direct user grants complement role grants for the admin flow:
-- surface -> module -> feature -> CRUD -> user.
CREATE SCHEMA IF NOT EXISTS authz;

CREATE TABLE IF NOT EXISTS authz.assignments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    principal_id uuid NOT NULL,
    permission text NOT NULL,
    campus_id uuid,
    department_id uuid,
    constraints jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS assignments_principal_idx
    ON authz.assignments (tenant_id, principal_id);

ALTER TABLE authz.assignments
    ADD COLUMN IF NOT EXISTS surface text NOT NULL DEFAULT 'app',
    ADD COLUMN IF NOT EXISTS scope text NOT NULL DEFAULT 'all',
    ADD COLUMN IF NOT EXISTS granted_by text NOT NULL DEFAULT 'system',
    ADD COLUMN IF NOT EXISTS active boolean NOT NULL DEFAULT true;

ALTER TABLE authz.assignments
    DROP CONSTRAINT IF EXISTS assignments_surface_check;
ALTER TABLE authz.assignments
    ADD CONSTRAINT assignments_surface_check CHECK (surface IN ('app', 'website'));

ALTER TABLE authz.assignments
    DROP CONSTRAINT IF EXISTS assignments_scope_check;
ALTER TABLE authz.assignments
    ADD CONSTRAINT assignments_scope_check CHECK (scope IN ('all', 'assigned', 'own'));

CREATE INDEX IF NOT EXISTS assignments_effective_access_idx
    ON authz.assignments (tenant_id, principal_id, surface, active);
