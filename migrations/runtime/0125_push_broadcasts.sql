-- Administrator push broadcasts.
--
-- A broadcast is one message an administrator sends to chosen roles and/or
-- chosen people. Every recipient gets their own row in
-- campus_ops.notifications (so the message is in the app inbox even when no
-- push provider is configured); the notification worker turns those rows into
-- FCM pushes when FCM_ENABLED=true. This table is the history the admin sees.
--
-- The API applies the same forward-only change at runtime
-- (ensure_push_broadcast_schema in push_broadcasts.rs) for databases the
-- migrator cannot reach. Safe to run more than once.
CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.push_broadcasts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 120),
    body text NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    image_url text,
    audience jsonb NOT NULL DEFAULT '{}'::jsonb,
    audience_summary text NOT NULL DEFAULT '',
    recipient_count integer NOT NULL DEFAULT 0 CHECK (recipient_count >= 0),
    push_recipient_count integer NOT NULL DEFAULT 0 CHECK (push_recipient_count >= 0),
    device_count integer NOT NULL DEFAULT 0 CHECK (device_count >= 0),
    push_status text NOT NULL DEFAULT 'queued'
        CHECK (push_status IN ('queued', 'not_configured', 'no_devices')),
    sent_by_user_id text NOT NULL,
    sent_by_name text NOT NULL DEFAULT '',
    sent_by_email text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS campus_ops_push_broadcasts_recent_idx
    ON campus_ops.push_broadcasts (tenant_id, created_at DESC);

-- History aggregates read and delivery counts per broadcast.
CREATE INDEX IF NOT EXISTS campus_ops_notifications_broadcast_idx
    ON campus_ops.notifications (tenant_id, (data ->> 'broadcastId'))
    WHERE category = 'broadcast';

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
VALUES
    ('notifications.broadcast.read', 'notifications', 'broadcast', 'read',
     'View push broadcasts',
     'See push reach (users, devices) and the history of broadcast notifications'),
    ('notifications.broadcast.send', 'notifications', 'broadcast', 'send',
     'Send push broadcasts',
     'Send a notification to chosen roles, people or students')
ON CONFLICT (permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,
    feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,
    display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,
    active=true,
    updated_at=now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name, description)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.display_name, template.description
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key IN ('notifications.broadcast.read', 'notifications.broadcast.send')
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,
    feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,
    display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,
    active=true,
    updated_at=now();

-- The institution's administrators hold both by default; any other role can
-- be given them from the access-control console.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by, surface)
SELECT role.tenant_id, role.id, permission.permission_key, 'institution',
       '{}'::jsonb, 'push-broadcasts-migration', surface.name
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id=role.tenant_id
 AND permission.permission_key IN ('notifications.broadcast.read', 'notifications.broadcast.send')
CROSS JOIN (VALUES ('website'::text), ('app'::text)) surface(name)
WHERE role.role_key IN ('tenant_admin', 'superadmin') AND role.active AND permission.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO NOTHING;
