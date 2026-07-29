CREATE SCHEMA IF NOT EXISTS library;
CREATE TABLE IF NOT EXISTS library.records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS library_records_tenant_idx ON library.records (tenant_id, record_type);
ALTER TABLE library.records ENABLE ROW LEVEL SECURITY;
CREATE POLICY library_records_tenant_isolation ON library.records
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);