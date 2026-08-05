CREATE TABLE IF NOT EXISTS crm.campaigns (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    name text NOT NULL,
    source text NOT NULL,
    budget numeric(14, 2) NOT NULL DEFAULT 0,
    spent numeric(14, 2) NOT NULL DEFAULT 0,
    attributed_revenue numeric(14, 2) NOT NULL DEFAULT 0,
    landing_pages integer NOT NULL DEFAULT 0,
    utm_code text,
    status text NOT NULL DEFAULT 'draft',
    starts_on date,
    ends_on date,
    updated_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name),
    CHECK (budget >= 0 AND spent >= 0 AND attributed_revenue >= 0),
    CHECK (landing_pages >= 0),
    CHECK (status IN ('draft', 'active', 'paused', 'completed'))
);

CREATE INDEX IF NOT EXISTS crm_campaigns_source_idx
    ON crm.campaigns (tenant_id, source, status);

ALTER TABLE crm.campaigns ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.campaigns;
CREATE POLICY tenant_isolation ON crm.campaigns
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

INSERT INTO crm.automation_toggles
    (tenant_id, stage, trigger_name, action, template_key, conditions, enabled, mandatory, updated_by)
SELECT tenant.id, automation.stage, automation.trigger_name, automation.action,
       automation.template_key, automation.conditions, automation.enabled,
       automation.mandatory, 'crm-module-migration-0002'
FROM platform.tenants AS tenant
CROSS JOIN (VALUES
    ('enquiry', 'on_create', 'auto_assign_digital_leads', NULL::text, '[]'::jsonb, true, false),
    ('enquiry', 'follow_up_due', 'send_follow_up_reminder', NULL::text, '[]'::jsonb, false, false),
    ('qualified', 'on_enter', 'send_whatsapp', 'qualified_confirmation', '[]'::jsonb, true, false),
    ('offer_status', 'offer_accepted', 'erp_handoff', NULL::text, '[]'::jsonb, false, false)
) AS automation(stage, trigger_name, action, template_key, conditions, enabled, mandatory)
ON CONFLICT (tenant_id, stage, trigger_name, action) DO NOTHING;
