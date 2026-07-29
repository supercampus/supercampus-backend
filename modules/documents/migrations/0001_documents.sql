CREATE SCHEMA IF NOT EXISTS documents;
CREATE TABLE IF NOT EXISTS documents.records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS documents_records_tenant_idx ON documents.records (tenant_id, record_type);
ALTER TABLE documents.records ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_records_tenant_isolation ON documents.records
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);