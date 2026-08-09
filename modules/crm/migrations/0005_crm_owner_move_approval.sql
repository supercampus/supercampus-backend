-- Module-local schema mirror of runtime migration 0024. Runtime RBAC catalog
-- grants remain in the platform migration because standalone CRM tests do not
-- install the platform authorization schema.
CREATE TABLE IF NOT EXISTS crm.lead_move_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    requested_by text NOT NULL,
    owner_id text NOT NULL,
    from_stage text NOT NULL,
    from_substate text NOT NULL,
    to_stage text NOT NULL,
    to_substate text NOT NULL,
    reason text,
    notes text,
    status text NOT NULL DEFAULT 'pending',
    decided_by text,
    decision_reason text,
    decided_at timestamptz,
    expires_at timestamptz NOT NULL DEFAULT (now() + interval '24 hours'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (status IN ('pending', 'approved', 'rejected', 'stale', 'expired'))
);
CREATE INDEX IF NOT EXISTS crm_lead_move_requests_owner_idx
    ON crm.lead_move_requests (tenant_id, owner_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS crm_lead_move_requests_requester_idx
    ON crm.lead_move_requests (tenant_id, requested_by, status, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS crm_lead_move_requests_pending_idx
    ON crm.lead_move_requests (tenant_id, lead_id, requested_by, to_stage, to_substate)
    WHERE status = 'pending';
ALTER TABLE crm.lead_move_requests ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.lead_move_requests;
CREATE POLICY tenant_isolation ON crm.lead_move_requests
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
