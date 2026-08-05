CREATE SCHEMA IF NOT EXISTS authz;
CREATE TABLE IF NOT EXISTS authz.assignments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    principal_id uuid NOT NULL,
    permission text NOT NULL,
    campus_id uuid,
    department_id uuid,
    constraints jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS assignments_principal_idx
    ON authz.assignments (tenant_id, principal_id);