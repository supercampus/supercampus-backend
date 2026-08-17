CREATE TABLE IF NOT EXISTS crm.lead_deletion_audit (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id),
    lead_snapshot jsonb NOT NULL,
    reason text NOT NULL CHECK (char_length(reason) BETWEEN 3 AND 500),
    deleted_by text NOT NULL,
    deleted_by_role text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS crm_lead_deletion_audit_tenant_created_idx
    ON crm.lead_deletion_audit (tenant_id, created_at DESC);

ALTER TABLE crm.lead_deletion_audit ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.lead_deletion_audit;
CREATE POLICY tenant_isolation ON crm.lead_deletion_audit
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

CREATE OR REPLACE FUNCTION crm.reject_lead_deletion_audit_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'crm.lead_deletion_audit is append-only';
END;
$$;

DROP TRIGGER IF EXISTS lead_deletion_audit_append_only ON crm.lead_deletion_audit;
CREATE TRIGGER lead_deletion_audit_append_only
    BEFORE UPDATE OR DELETE ON crm.lead_deletion_audit
    FOR EACH ROW EXECUTE FUNCTION crm.reject_lead_deletion_audit_mutation();
