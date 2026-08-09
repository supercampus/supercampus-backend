CREATE TABLE IF NOT EXISTS crm.lead_application_links (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    case_id text NOT NULL,
    application_id text NOT NULL,
    admission_id text NOT NULL,
    application_status text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, lead_id),
    UNIQUE (tenant_id, case_id),
    UNIQUE (tenant_id, application_id)
);

ALTER TABLE crm.lead_application_links ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.lead_application_links;
CREATE POLICY tenant_isolation ON crm.lead_application_links
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
