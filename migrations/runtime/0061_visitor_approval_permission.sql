-- `gatepass.visitor.approve`.
--
-- Visitor passes wait on an administrator before a QR is minted and sent, and
-- the vocabulary had no key for that decision — only `visitor.create` and
-- `visitor.read`. Borrowing `outpass.approve` would have handed visitor
-- approval to every warden and class advisor who can release a student for the
-- afternoon, which is a different judgement about a different person.
--
-- Templates seed new tenants; the second statement backfills the tenants that
-- already exist, since authz.permission_definitions is per-tenant and the
-- bootstrap trigger only fires on insert.

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES (
    'gatepass.visitor.approve', 'gatepass', 'visitor', 'approve', ARRAY['update'],
    'Approve visitor passes',
    'Approve or reject a parent or guest visit, issuing the gate QR',
    true
)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true;

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions, display_name, description)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description
FROM platform.tenants tenant
CROSS JOIN authz.permission_templates template
WHERE template.permission_key = 'gatepass.visitor.approve'
ON CONFLICT (tenant_id, permission_key) DO NOTHING;
