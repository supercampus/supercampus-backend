CREATE SCHEMA IF NOT EXISTS fees;
CREATE TABLE IF NOT EXISTS fees.records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS fees_records_tenant_idx ON fees.records (tenant_id, record_type);
ALTER TABLE fees.records ENABLE ROW LEVEL SECURITY;
CREATE POLICY fees_records_tenant_isolation ON fees.records
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);