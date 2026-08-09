-- Collaborative CRM pipeline ownership.
-- The first successful movement out of Enquiry claims the lead. Later movements
-- by another user require a one-use approval from the current owner.
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

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT permission_key, 'crm', 'lead_move_requests', action, ARRAY[crud_action]::text[],
       display_name, description, true
FROM (VALUES
    ('crm.leads.stage.request', 'create', 'create', 'Request lead movement', 'Request a one-use stage movement from the current lead owner'),
    ('crm.leads.stage.approve', 'approve', 'update', 'Approve lead movement', 'Approve or reject movement requests for owned leads'),
    ('crm.leads.stage.override', 'override', 'update', 'Override lead ownership', 'Move another user''s lead without owner approval'),
    ('crm.leads.stage.backward', 'backward', 'update', 'Move leads backward', 'Move a lead backward through the pipeline')
) AS permission(permission_key, action, crud_action, display_name, description)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name,
       template.description, true
FROM platform.tenants AS tenant
CROSS JOIN authz.permission_templates AS template
WHERE template.permission_key IN (
    'crm.leads.stage.request', 'crm.leads.stage.approve',
    'crm.leads.stage.override', 'crm.leads.stage.backward'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- Existing stage movers can request and decide moves. The protected tenant admin
-- additionally receives explicit override/backward grants; no role-name checks are
-- performed by application code.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT existing.tenant_id, existing.role_id, permission.permission_key,
       existing.scope, '{}'::jsonb, 'runtime-migration-0024'
FROM authz.role_permissions AS existing
CROSS JOIN (VALUES ('crm.leads.stage.request'), ('crm.leads.stage.approve'))
    AS permission(permission_key)
WHERE existing.permission_key = 'crm.leads.stage.move'
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT role.tenant_id, role.id, permission.permission_key, 'all', '{}'::jsonb,
       'runtime-migration-0024'
FROM authz.roles AS role
CROSS JOIN (VALUES ('crm.leads.stage.override'), ('crm.leads.stage.backward'))
    AS permission(permission_key)
WHERE role.protected
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;
