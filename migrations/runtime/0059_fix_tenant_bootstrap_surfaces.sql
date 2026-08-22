-- 0048 widened authz.role_permissions' primary key to include `surface` and
-- backfilled every existing role across both surfaces. The bootstrap trigger
-- introduced in 0011 was never brought along: it still names the pre-0048
-- conflict target (tenant_id, role_id, permission_key), which no longer matches
-- a unique index. Postgres rejects the statement outright, so the trigger
-- aborts every INSERT into platform.tenants — creating a tenant has been
-- impossible since 0048 shipped.
--
-- Two things change here. The conflict target gains `surface`, and the wildcard
-- grant is written for both surfaces rather than falling through to the column
-- default of 'website' — a freshly bootstrapped tenant admin locked out of the
-- app surface is exactly what 0048 and 0049 set out to prevent. The new tenant
-- admin is also registered in authz.role_surfaces, which 0048 backfilled for
-- roles that already existed but left unpopulated for roles created later.

CREATE OR REPLACE FUNCTION authz.bootstrap_tenant_permissions()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    INSERT INTO authz.permission_definitions
        (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
         display_name, description)
    SELECT NEW.id, template.permission_key, template.module_key, template.feature_key,
           template.action, template.crud_actions, template.display_name,
           template.description
    FROM authz.permission_templates AS template
    WHERE template.active
    ON CONFLICT (tenant_id, permission_key) DO NOTHING;

    INSERT INTO authz.roles
        (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
    VALUES (NEW.id, 'tenant_admin', 'Tenant Admin', 'Administration',
            'Controls tenant users, roles, permissions, settings, and modules', true,
            'tenant-bootstrap', 'tenant-bootstrap')
    ON CONFLICT (tenant_id, role_key) DO NOTHING;

    INSERT INTO authz.role_permissions
        (tenant_id, role_id, surface, permission_key, scope, granted_by)
    SELECT NEW.id, role.id, available.surface, '*', 'all', 'tenant-bootstrap'
    FROM authz.roles AS role
    CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
    WHERE role.tenant_id = NEW.id AND role.role_key = 'tenant_admin'
    ON CONFLICT (tenant_id, role_id, surface, permission_key) DO NOTHING;

    INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
    SELECT NEW.id, role.id, available.surface, 'tenant-bootstrap'
    FROM authz.roles AS role
    CROSS JOIN (VALUES ('website'::text), ('app'::text)) AS available(surface)
    WHERE role.tenant_id = NEW.id AND role.role_key = 'tenant_admin'
    ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

    RETURN NEW;
END;
$function$;
