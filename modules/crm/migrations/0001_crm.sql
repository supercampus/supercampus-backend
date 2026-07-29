CREATE SCHEMA IF NOT EXISTS crm;
CREATE TABLE IF NOT EXISTS crm.leads (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    full_name text NOT NULL,
    email text,
    pipeline_key text NOT NULL,
    stage_key text NOT NULL,
    custom_fields jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS crm_leads_pipeline_idx
    ON crm.leads (tenant_id, pipeline_key, stage_key);
ALTER TABLE crm.leads ENABLE ROW LEVEL SECURITY;
CREATE POLICY crm_leads_tenant_isolation ON crm.leads
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);