CREATE SCHEMA IF NOT EXISTS admissions;
CREATE TABLE IF NOT EXISTS admissions.records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS admissions_records_tenant_idx ON admissions.records (tenant_id, record_type);
ALTER TABLE admissions.records ENABLE ROW LEVEL SECURITY;
CREATE POLICY admissions_records_tenant_isolation ON admissions.records
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);