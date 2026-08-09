-- CRM leads always enter the shared Enquiry pool. Assignment is an explicit,
-- authenticated self-claim (or a separately authorised manager assignment).
UPDATE crm.automation_toggles
SET enabled = false,
    updated_at = now()
WHERE action = 'auto_assign_digital_leads';

CREATE INDEX IF NOT EXISTS crm_unassigned_enquiry_pool_idx
    ON crm.leads (tenant_id, created_at, id)
    WHERE deleted_at IS NULL
      AND stage_key = 'enquiry'
      AND assigned_to IS NULL;

ALTER TABLE crm.form_submissions
    ADD COLUMN IF NOT EXISTS campaign_id uuid REFERENCES crm.campaigns(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS idempotency_key text,
    ADD COLUMN IF NOT EXISTS processing_status text NOT NULL DEFAULT 'processed',
    ADD COLUMN IF NOT EXISTS processing_error text,
    ADD COLUMN IF NOT EXISTS processed_at timestamptz DEFAULT now();

CREATE UNIQUE INDEX IF NOT EXISTS crm_form_submission_idempotency_idx
    ON crm.form_submissions (tenant_id, form_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS crm.campaign_forms (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    campaign_id uuid NOT NULL REFERENCES crm.campaigns(id) ON DELETE CASCADE,
    form_id uuid NOT NULL REFERENCES crm.forms(id) ON DELETE CASCADE,
    is_primary boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, campaign_id, form_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS crm_campaign_primary_form_idx
    ON crm.campaign_forms (tenant_id, campaign_id)
    WHERE is_primary;

ALTER TABLE crm.campaign_forms ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.campaign_forms;
CREATE POLICY tenant_isolation ON crm.campaign_forms
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

CREATE TABLE IF NOT EXISTS crm.automation_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    stage text NOT NULL,
    trigger_name text NOT NULL,
    action text NOT NULL,
    template_key text,
    status text NOT NULL,
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (status IN ('queued', 'completed', 'failed', 'skipped'))
);

CREATE INDEX IF NOT EXISTS crm_automation_runs_lead_idx
    ON crm.automation_runs (tenant_id, lead_id, created_at DESC);

ALTER TABLE crm.automation_runs ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.automation_runs;
CREATE POLICY tenant_isolation ON crm.automation_runs
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
