CREATE SCHEMA IF NOT EXISTS placement;
CREATE TABLE IF NOT EXISTS placement.records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS placement_records_tenant_idx ON placement.records (tenant_id, record_type);
ALTER TABLE placement.records ENABLE ROW LEVEL SECURITY;
CREATE POLICY placement_records_tenant_isolation ON placement.records
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);