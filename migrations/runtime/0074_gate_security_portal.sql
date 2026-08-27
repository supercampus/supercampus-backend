-- The mobile gate-security portal records entry/exit scans and reads the
-- resulting movement log. Keep this grant tenant-generic; individual tenants
-- may name their security staff differently while retaining the security role.
INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, 'app', 'runtime-migration-0074'
FROM authz.roles role
WHERE role.role_key = 'security' AND role.active
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints,
     granted_by, granted_at)
SELECT role.tenant_id, role.id, definition.permission_key, 'app',
       'institution', '{}'::jsonb, 'runtime-migration-0074', now()
FROM authz.roles role
JOIN authz.permission_definitions definition
  ON definition.tenant_id = role.tenant_id
 AND definition.permission_key IN (
     'gatepass.scan.create', 'gatepass.scan.read', 'gatepass.access.read'
 )
 AND definition.active
WHERE role.role_key = 'security' AND role.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = EXCLUDED.granted_at;

-- MEC test identity. It deliberately reuses the institution's current shared
-- test-password hash; production deployments should replace the credential.
DO $$
DECLARE
    mec_tenant_id uuid;
    security_role_id uuid;
    shared_password_hash text;
    security_user_id uuid := '1c7310f7-f249-58b5-9946-fde611374014'::uuid;
BEGIN
    SELECT id INTO mec_tenant_id
      FROM platform.tenants
     WHERE slug = 'mec';
    IF mec_tenant_id IS NULL THEN
        RETURN;
    END IF;

    SELECT password_hash INTO shared_password_hash
      FROM identity.users
     WHERE email = 'principal@mec.local';
    IF shared_password_hash IS NULL THEN
        RETURN;
    END IF;

    INSERT INTO identity.users
        (id, email, password_hash, display_name, initials, account_type,
         active, profile)
    VALUES
        (security_user_id, 'security@mec.local', shared_password_hash,
         'MEC Gate Security', 'GS', 'staff', true,
         '{"designation":"Gate Security","team":"Campus Security","dept":"Main Gate"}'::jsonb)
    ON CONFLICT (id) DO UPDATE SET
        email = EXCLUDED.email,
        password_hash = EXCLUDED.password_hash,
        display_name = EXCLUDED.display_name,
        initials = EXCLUDED.initials,
        account_type = EXCLUDED.account_type,
        active = true,
        profile = EXCLUDED.profile,
        updated_at = now();

    INSERT INTO identity.tenant_memberships
        (tenant_id, user_id, roles, active, is_primary, profile)
    VALUES
        (mec_tenant_id, security_user_id, ARRAY['security']::text[], true,
         true,
         '{"designation":"Gate Security","team":"Campus Security","dept":"Main Gate"}'::jsonb)
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        roles = EXCLUDED.roles,
        active = true,
        is_primary = true,
        profile = EXCLUDED.profile,
        updated_at = now();

    SELECT id INTO security_role_id
      FROM authz.roles
     WHERE tenant_id = mec_tenant_id AND role_key = 'security' AND active
     LIMIT 1;

    IF security_role_id IS NOT NULL THEN
        DELETE FROM authz.user_roles
         WHERE tenant_id = mec_tenant_id
           AND user_id = security_user_id
           AND role_id <> security_role_id;
        INSERT INTO authz.user_roles
            (tenant_id, user_id, role_id, assigned_by)
        VALUES
            (mec_tenant_id, security_user_id, security_role_id,
             'runtime-migration-0074')
        ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;
    END IF;

    UPDATE identity.auth_sessions
       SET revoked_at = COALESCE(revoked_at, now())
     WHERE tenant_id = mec_tenant_id
       AND user_id = security_user_id::text;
END $$;
