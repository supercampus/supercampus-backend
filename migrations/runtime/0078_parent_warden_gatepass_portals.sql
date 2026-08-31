-- Signed-in parent and warden portals for the MEC outpass approval matrix.
-- Idempotent so the isolated production command may be safely retried.

ALTER TABLE campus_ops.gatepass_requests
    ADD COLUMN IF NOT EXISTS qr_payload text;

DO $$
DECLARE
    mec_tenant uuid;
    parent_id uuid;
    warden_id uuid;
    child_user_id text;
BEGIN
    SELECT id INTO mec_tenant FROM platform.tenants WHERE slug = 'mec';
    IF mec_tenant IS NULL THEN
        RETURN;
    END IF;

    INSERT INTO identity.users
        (email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('selvamoorthy@gmail.com', crypt('Mec@2026', gen_salt('bf', 12)),
         'Selvamoorthy', 'S', 'parent', true,
         '{"relationship":"Parent","team":"Parents"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
        password_hash = EXCLUDED.password_hash,
        display_name = EXCLUDED.display_name,
        initials = EXCLUDED.initials,
        account_type = EXCLUDED.account_type,
        active = true,
        profile = identity.users.profile || EXCLUDED.profile,
        updated_at = now()
    RETURNING id INTO parent_id;

    INSERT INTO identity.tenant_memberships
        (tenant_id, user_id, roles, active, is_primary, profile)
    VALUES
        (mec_tenant, parent_id, ARRAY['parent']::text[], true, true,
         '{"relationship":"Parent","team":"Parents"}'::jsonb)
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        roles = EXCLUDED.roles, active = true, is_primary = true,
        profile = identity.tenant_memberships.profile || EXCLUDED.profile,
        updated_at = now();

    INSERT INTO identity.users
        (email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('warden@mec.local', crypt('Mec@2026', gen_salt('bf', 12)),
         'MEC Hostel Warden', 'MW', 'staff', true,
         '{"designation":"Hostel Warden","team":"Hostel"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
        password_hash = EXCLUDED.password_hash,
        display_name = EXCLUDED.display_name,
        initials = EXCLUDED.initials,
        account_type = EXCLUDED.account_type,
        active = true,
        profile = identity.users.profile || EXCLUDED.profile,
        updated_at = now()
    RETURNING id INTO warden_id;

    INSERT INTO identity.tenant_memberships
        (tenant_id, user_id, roles, active, is_primary, profile)
    VALUES
        (mec_tenant, warden_id, ARRAY['warden']::text[], true, true,
         '{"designation":"Hostel Warden","team":"Hostel"}'::jsonb)
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        roles = EXCLUDED.roles, active = true, is_primary = true,
        profile = identity.tenant_memberships.profile || EXCLUDED.profile,
        updated_at = now();

    -- These two portals are intentionally enabled only on the app surface.
    INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
    SELECT mec_tenant, role.id, 'app', 'runtime-migration-0078'
    FROM authz.roles role
    WHERE role.tenant_id = mec_tenant AND role.role_key IN ('parent', 'warden')
    ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

    INSERT INTO authz.role_permissions
        (tenant_id, role_id, surface, permission_key, scope, constraints, granted_by)
    SELECT mec_tenant, role.id, 'app', permission.permission_key,
           CASE WHEN role.role_key = 'parent' THEN 'own' ELSE 'assigned' END,
           '{}'::jsonb, 'runtime-migration-0078'
    FROM authz.roles role
    JOIN (VALUES
        ('parent', 'gatepass.outpass.read'),
        ('parent', 'gatepass.outpass.approve'),
        ('parent', 'attendance.parent.read'),
        ('warden', 'gatepass.outpass.read'),
        ('warden', 'gatepass.outpass.approve')
    ) AS permission(role_key, permission_key)
      ON permission.role_key = role.role_key
    WHERE role.tenant_id = mec_tenant
    ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
        scope = EXCLUDED.scope,
        constraints = EXCLUDED.constraints,
        granted_by = EXCLUDED.granted_by,
        granted_at = now();

    SELECT student.user_account_id::text
      INTO child_user_id
      FROM core.students student
     WHERE student.tenant_id = mec_tenant
       AND student.user_account_id IS NOT NULL
       AND (
         upper(student.student_number) = 'MEC25AD48'
         OR lower(student.full_name) IN ('vishnu s', 'vishnu sudharshan')
       )
     ORDER BY CASE WHEN upper(student.student_number) = 'MEC25AD48' THEN 0 ELSE 1 END
     LIMIT 1;

    IF child_user_id IS NOT NULL THEN
        INSERT INTO campus_ops.parent_student_links
            (tenant_id, parent_user_id, student_user_id, active)
        VALUES (mec_tenant, parent_id::text, child_user_id, true)
        ON CONFLICT (tenant_id, parent_user_id, student_user_id)
        DO UPDATE SET active = true;
    END IF;
END $$;
