-- Minimum and latest supported app versions, per tenant and platform.
--
-- The mobile app reads the policy for its platform at startup and on resume
-- (GET /api/app-version, unauthenticated) and compares it with its own
-- version: below minimum_version, or below latest_version with force_update
-- on, it blocks with a link to store_url; below latest_version otherwise it
-- shows a dismissible prompt. Web builds are always current and skip it.
--
-- Stored per tenant because each institution's administrator manages its own
-- rollout from the Admin Desk; the public endpoint reads the signed-in
-- tenant's policy, or the deployment's primary tenant before sign-in.
--
-- The seed matches the current release (1.0.9) as latest and 1.0.0 as the
-- minimum, so nobody already installed is blocked by this migration.
--
-- The API applies this file at runtime (app_versions::ensure_schema) for
-- databases the migrator cannot reach. Safe to run more than once.
CREATE TABLE IF NOT EXISTS platform.app_version_policies (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    platform text NOT NULL CHECK (platform IN ('android', 'ios')),
    latest_version text NOT NULL,
    minimum_version text NOT NULL,
    store_url text NOT NULL DEFAULT '',
    force_update boolean NOT NULL DEFAULT false,
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, platform)
);

INSERT INTO platform.app_version_policies
    (tenant_id, platform, latest_version, minimum_version, store_url, force_update, updated_by)
SELECT tenant.id, seed.platform, '1.0.9', '1.0.0', seed.store_url, false, 'seed'
FROM platform.tenants tenant
CROSS JOIN (VALUES
    ('android'::text, 'https://play.google.com/store/apps/details?id=ai.supercampus.mobile'::text),
    ('ios'::text, ''::text)
) AS seed(platform, store_url)
WHERE tenant.slug <> 'supercampus-control'
ON CONFLICT (tenant_id, platform) DO NOTHING;

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT permission_key, module_key, feature_key, action, crud_actions, display_name, description, true
FROM (VALUES
    ('administration.app_versions.read', 'administration', 'app_versions', 'read', ARRAY['read']::text[],
     'View app versions', 'See the minimum and latest supported app versions'),
    ('administration.app_versions.update', 'administration', 'app_versions', 'update',
     ARRAY['update']::text[], 'Manage app versions',
     'Set the minimum and latest app versions, store links and force update')
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
    'administration.app_versions.read',
    'administration.app_versions.update'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();
