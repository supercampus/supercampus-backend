-- Canteen captains are order-only counter operators. They can see their
-- assigned shop's live queue and analytics, advance/reject orders, inspect
-- history, and switch their own eat/work state. They cannot edit shops or menu.
INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, portal_family,
     protected, created_by, updated_by)
SELECT tenant.id, 'canteen_captain', 'Canteen captain', 'Canteen',
       'Handles the live order queue for assigned canteen shops', 'staff',
       false, 'runtime-migration-0075', 'runtime-migration-0075'
FROM platform.tenants tenant
ON CONFLICT (tenant_id, role_key) DO UPDATE SET
    name = EXCLUDED.name,
    team = EXCLUDED.team,
    scope_description = EXCLUDED.scope_description,
    portal_family = 'staff',
    active = true,
    updated_by = 'runtime-migration-0075',
    updated_at = now();

INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
SELECT role.tenant_id, role.id, 'app', 'runtime-migration-0075'
FROM authz.roles role
WHERE role.role_key = 'canteen_captain' AND role.active
ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints,
     granted_by, granted_at)
SELECT role.tenant_id, role.id, definition.permission_key, 'app',
       'assigned', '{}'::jsonb, 'runtime-migration-0075', now()
FROM authz.roles role
JOIN authz.permission_definitions definition
  ON definition.tenant_id = role.tenant_id
 AND definition.permission_key IN (
     'canteen.menu.read',
     'canteen.order.read',
     'canteen.orders.manage',
     'canteen.analytics.read'
 )
 AND definition.active
WHERE role.role_key = 'canteen_captain' AND role.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = EXCLUDED.granted_at;

DO $$
DECLARE
    mec_tenant_id uuid;
    captain_role_id uuid;
    shared_password_hash text;
    canteen_shop_id uuid;
    captain record;
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
        RAISE EXCEPTION 'principal identity is required before adding MEC canteen captains';
    END IF;

    SELECT id INTO captain_role_id
      FROM authz.roles
     WHERE tenant_id = mec_tenant_id
       AND role_key = 'canteen_captain'
       AND active
     LIMIT 1;

    -- Prefer the explicitly configured MEC canteen, while remaining compatible
    -- with installations whose original shop still uses the classic key.
    SELECT id INTO canteen_shop_id
      FROM campus_ops.shops
     WHERE tenant_id = mec_tenant_id
       AND is_active
       AND lower(category) = 'canteen'
     ORDER BY CASE
       WHEN shop_key = 'mec-canteen' THEN 0
       WHEN lower(name) = 'canteen' THEN 1
       WHEN shop_key = 'classic' THEN 2
       ELSE 3
     END, created_at
     LIMIT 1;
    IF canteen_shop_id IS NULL THEN
        RAISE EXCEPTION 'MEC needs an active canteen shop before adding captains';
    END IF;

    FOR captain IN
        SELECT * FROM (VALUES
            ('337c8d72-79b0-5dd4-a311-fbd72cbe9f01'::uuid, 'shashi@mec.local',   'Shashi',   'S',  'MECCAP001'),
            ('b0db044a-7a27-5ab6-9e59-d2b3df42b202'::uuid, 'purusoth@mec.local', 'Purusoth', 'P',  'MECCAP002'),
            ('f235af7d-b947-5d5d-94b4-fc95f6f5d303'::uuid, 'kesava@mec.local',   'Kesava',   'K',  'MECCAP003'),
            ('a3e190a4-4f6f-59f2-b2f1-8768bc2cd404'::uuid, 'yuvaraj@mec.local',  'Yuvaraj',  'Y',  'MECCAP004')
        ) AS roster(user_id, email, full_name, initials, employee_number)
    LOOP
        INSERT INTO identity.users
            (id, email, password_hash, display_name, initials, account_type,
             active, profile)
        VALUES
            (captain.user_id, captain.email, shared_password_hash,
             captain.full_name, captain.initials, 'staff', true,
             jsonb_build_object('designation', 'Canteen Captain',
                                'team', 'Canteen', 'dept', 'Canteen'))
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
            (mec_tenant_id, captain.user_id,
             ARRAY['canteen_captain']::text[], true, true,
             jsonb_build_object('designation', 'Canteen Captain',
                                'team', 'Canteen', 'dept', 'Canteen'))
        ON CONFLICT (tenant_id, user_id) DO UPDATE SET
            roles = EXCLUDED.roles,
            active = true,
            is_primary = true,
            profile = EXCLUDED.profile,
            updated_at = now();

        IF captain_role_id IS NOT NULL THEN
            DELETE FROM authz.user_roles
             WHERE tenant_id = mec_tenant_id
               AND user_id = captain.user_id
               AND role_id <> captain_role_id;
            INSERT INTO authz.user_roles
                (tenant_id, user_id, role_id, assigned_by)
            VALUES
                (mec_tenant_id, captain.user_id, captain_role_id,
                 'runtime-migration-0075')
            ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;
        END IF;

        INSERT INTO core.employees
            (id, tenant_id, user_id, employee_number, full_name, email,
             status, profile)
        VALUES
            (captain.user_id, mec_tenant_id, captain.user_id,
             captain.employee_number, captain.full_name, captain.email,
             'active',
             jsonb_build_object('designation', 'Canteen Captain',
                                'team', 'Canteen', 'dept', 'Canteen'))
        ON CONFLICT (tenant_id, user_id) DO UPDATE SET
            employee_number = EXCLUDED.employee_number,
            full_name = EXCLUDED.full_name,
            email = EXCLUDED.email,
            status = 'active',
            profile = EXCLUDED.profile,
            updated_at = now();

        INSERT INTO campus_ops.shop_user_assignments
            (tenant_id, shop_id, user_id, assignment_role, is_active,
             assigned_by)
        VALUES
            (mec_tenant_id, canteen_shop_id, captain.user_id::text,
             'captain', true, 'runtime-migration-0075')
        ON CONFLICT (tenant_id, shop_id, user_id) DO UPDATE SET
            assignment_role = 'captain',
            is_active = true,
            assigned_by = 'runtime-migration-0075',
            updated_at = now();

        INSERT INTO campus_ops.canteen_staff_state
            (tenant_id, user_id, mode, shop_open)
        VALUES (mec_tenant_id, captain.user_id::text, 'work', NULL)
        ON CONFLICT (tenant_id, user_id) DO NOTHING;

        UPDATE identity.auth_sessions
           SET revoked_at = COALESCE(revoked_at, now())
         WHERE tenant_id = mec_tenant_id
           AND user_id = captain.user_id::text;
    END LOOP;
END $$;
