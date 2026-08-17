CREATE TABLE IF NOT EXISTS crm.application_invitations (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    form_id uuid NOT NULL REFERENCES crm.forms(id) ON DELETE RESTRICT,
    token uuid NOT NULL UNIQUE,
    otp_hash text NOT NULL,
    verification_token_hash text,
    channel text NOT NULL,
    contact text NOT NULL,
    status text NOT NULL DEFAULT 'issued',
    attempts integer NOT NULL DEFAULT 0,
    expires_at timestamptz NOT NULL,
    verified_at timestamptz,
    submitted_at timestamptz,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (channel IN ('whatsapp', 'sms')),
    CHECK (status IN ('issued', 'verified', 'submitted', 'expired', 'revoked')),
    CHECK (attempts >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS application_invitation_active_lead_idx
    ON crm.application_invitations (tenant_id, lead_id)
    WHERE status IN ('issued', 'verified');

CREATE INDEX IF NOT EXISTS application_invitation_token_idx
    ON crm.application_invitations (tenant_id, token);

ALTER TABLE crm.application_invitations ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.application_invitations;
CREATE POLICY tenant_isolation ON crm.application_invitations
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

-- The invitation service owns the Qualified message because it must include
-- the tenant application URL and OTP. Disable the old generic confirmation to
-- avoid sending applicants two messages for the same stage change.
UPDATE crm.automation_toggles
SET enabled = false, updated_at = now(), updated_by = 'runtime-migration-0038'
WHERE stage = 'qualified'
  AND trigger_name = 'on_enter'
  AND action = 'send_whatsapp'
  AND template_key = 'qualified_confirmation';
