-- Security logs and finance audit for the tenant Admin Desk.
--
-- identity.auth_login_events records what identity.auth_sessions cannot:
-- failed sign-ins, sign-ins refused because the account is active on another
-- device or the tenant is in maintenance, sign-outs, and sessions an
-- administrator revoked, together with the device, platform and client IP of
-- each attempt. Rows are written best-effort by the login/logout handlers and
-- never change how authentication behaves. Failed attempts are recorded only
-- when the email belongs to an existing account (so the row can be scoped to
-- that account's tenant); unknown emails are not stored.
--
-- The permission grants are new Admin Desk features. Tenant admins already
-- hold '*'; nothing else is granted by default.
--
-- The API applies this file at runtime (security_logs::ensure_schema) for
-- databases the migrator cannot reach. Safe to run more than once.
CREATE TABLE IF NOT EXISTS identity.auth_login_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text,
    email text,
    outcome text NOT NULL
        CHECK (outcome IN ('success', 'failure', 'blocked', 'signed_out', 'revoked')),
    reason text,
    session_id uuid,
    device_id text,
    device_name text,
    platform text,
    app_version text,
    ip_address text,
    user_agent text,
    actor_user_id text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS auth_login_events_tenant_created_idx
    ON identity.auth_login_events (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS auth_login_events_session_idx
    ON identity.auth_login_events (session_id)
    WHERE session_id IS NOT NULL;

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT permission_key, module_key, feature_key, action, crud_actions, display_name, description, true
FROM (VALUES
    ('administration.audit_logs.read', 'administration', 'audit_logs', 'read', ARRAY['read']::text[],
     'View finance audit logs',
     'See every wallet top-up, deduction, refund, order and laundry payment, payment request and online payment'),
    ('administration.security_logs.read', 'administration', 'security_logs', 'read', ARRAY['read']::text[],
     'View security logs', 'See sign-ins, failed attempts, sign-outs and login sessions'),
    ('administration.security_logs.revoke', 'administration', 'security_logs', 'revoke',
     ARRAY['update']::text[], 'Revoke login sessions', 'Sign a member out of their active session')
) AS permission(permission_key, module_key, feature_key, action, crud_actions, display_name, description)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key IN (
    'administration.audit_logs.read',
    'administration.security_logs.read',
    'administration.security_logs.revoke'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();
