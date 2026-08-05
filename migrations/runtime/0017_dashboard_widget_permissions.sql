-- Dynamic staff dashboard: one read-only permission per gated widget.
-- Tenant admins grant/revoke widgets per role through the existing Access
-- Control editor (the catalog-driven role UI groups these under "dashboard").
-- Backfill grants preserve today's hardcoded gates for existing roles.

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT permission_key, module_key, feature_key, action, ARRAY['read']::text[],
       display_name, description, true
FROM (VALUES
    ('dashboard.counselor_sla.read', 'dashboard', 'counselor_sla',
     'View counselor SLA widget', 'Show the counselor SLA card on the staff dashboard'),
    ('dashboard.track_team.read', 'dashboard', 'track_team',
     'View team tracking widget', 'Show the team tracking donut on the staff dashboard'),
    ('dashboard.pipeline_spread.read', 'dashboard', 'pipeline_spread',
     'View pipeline spread widget', 'Show the lead-by-stage chart on the staff dashboard'),
    ('dashboard.follow_ups.read', 'dashboard', 'follow_ups',
     'View follow-ups widget', 'Show the follow-ups list on the staff dashboard'),
    ('dashboard.fee_readiness.read', 'dashboard', 'fee_readiness',
     'View fee readiness widget', 'Show the fee readiness card on the staff dashboard'),
    ('dashboard.source_quality.read', 'dashboard', 'source_quality',
     'View source quality widget', 'Show the source quality card on the staff dashboard')
) AS permission(permission_key, module_key, feature_key, action, display_name, description)
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
WHERE template.module_key = 'dashboard'
  AND template.action = 'read'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- Preserve current visibility: grant each widget permission to roles that
-- already hold the legacy functional permission gating that widget today.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, granted_by)
SELECT rp.tenant_id, rp.role_id, widget.permission_key, 'all', 'migration-0017'
FROM authz.role_permissions AS rp
JOIN (VALUES
    ('crm.assignment.read', 'dashboard.counselor_sla.read'),
    ('crm.assignment.read', 'dashboard.track_team.read'),
    ('crm.leads.read', 'dashboard.pipeline_spread.read'),
    ('crm.leads.read', 'dashboard.follow_ups.read'),
    ('crm.erp.handoff', 'dashboard.fee_readiness.read'),
    ('crm.reports.read', 'dashboard.source_quality.read')
) AS widget(legacy_key, permission_key)
    ON widget.legacy_key = rp.permission_key
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;

-- Default per-tenant dashboard definition. Ungated widgets keep
-- "requiredPermission": null so they render for every role, as today.
INSERT INTO configuration.runtime_documents (tenant_id, namespace, version, value)
SELECT tenant.id, 'dashboard.staff', 1, jsonb_build_object('widgets', jsonb_build_array(
    jsonb_build_object('id', 'profile', 'enabled', true, 'requiredPermission', null),
    jsonb_build_object('id', 'avg_response_time', 'enabled', true, 'requiredPermission', null),
    jsonb_build_object('id', 'admission_velocity', 'enabled', true, 'requiredPermission', null),
    jsonb_build_object('id', 'counselor_sla', 'enabled', true, 'requiredPermission', 'dashboard.counselor_sla.read'),
    jsonb_build_object('id', 'track_team', 'enabled', true, 'requiredPermission', 'dashboard.track_team.read'),
    jsonb_build_object('id', 'talent_recruitment', 'enabled', true, 'requiredPermission', null),
    jsonb_build_object('id', 'pipeline_spread', 'enabled', true, 'requiredPermission', 'dashboard.pipeline_spread.read'),
    jsonb_build_object('id', 'follow_ups', 'enabled', true, 'requiredPermission', 'dashboard.follow_ups.read'),
    jsonb_build_object('id', 'fee_readiness', 'enabled', true, 'requiredPermission', 'dashboard.fee_readiness.read'),
    jsonb_build_object('id', 'source_quality', 'enabled', true, 'requiredPermission', 'dashboard.source_quality.read')
))
FROM platform.tenants AS tenant
ON CONFLICT (tenant_id, namespace) DO NOTHING;
