-- MEC librarian and stationery-only operator identities and app permissions.

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description,
     crud_actions, active)
VALUES
    ('library.visit_pass.approve', 'library', 'visit_pass', 'approve',
     'Approve library visit', 'Validate and approve or reject library visit QR requests',
     ARRAY['update']::text[], true)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name,
     description, crud_actions, active)
SELECT tenant.id, template.permission_key, template.module_key,
       template.feature_key, template.action, template.display_name,
       template.description, template.crud_actions, true
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key = 'library.visit_pass.approve'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

DO $$
DECLARE
    mec_tenant uuid;
    librarian_id uuid;
    stationery_id uuid;
    librarian_role uuid;
    stationery_role uuid;
    password_hash text;
BEGIN
    SELECT id INTO mec_tenant FROM platform.tenants WHERE slug = 'mec';
    IF mec_tenant IS NULL THEN RETURN; END IF;

    SELECT identity.users.password_hash INTO password_hash
      FROM identity.users WHERE email = 'principal@mec.local';
    IF password_hash IS NULL THEN
        password_hash := crypt('Mec@2026', gen_salt('bf', 12));
    END IF;

    INSERT INTO authz.roles
        (tenant_id, role_key, name, team, scope_description, portal_family,
         protected, active, created_by, updated_by)
    VALUES
        (mec_tenant, 'librarian', 'Librarian', 'Library',
         'Validates library visit QR requests and approves or rejects them',
         'staff', false, true, 'runtime-migration-0079', 'runtime-migration-0079'),
        (mec_tenant, 'stationery_operator', 'Stationery operator', 'Stationery',
         'Operates only the assigned campus stationery shop',
         'staff', false, true, 'runtime-migration-0079', 'runtime-migration-0079')
    ON CONFLICT (tenant_id, role_key) DO UPDATE SET
        name = EXCLUDED.name, team = EXCLUDED.team,
        scope_description = EXCLUDED.scope_description,
        portal_family = 'staff', active = true,
        updated_by = EXCLUDED.updated_by, updated_at = now();

    SELECT id INTO librarian_role FROM authz.roles
     WHERE tenant_id = mec_tenant AND role_key = 'librarian';
    SELECT id INTO stationery_role FROM authz.roles
     WHERE tenant_id = mec_tenant AND role_key = 'stationery_operator';

    INSERT INTO authz.role_surfaces (tenant_id, role_id, surface, enabled_by)
    VALUES
        (mec_tenant, librarian_role, 'app', 'runtime-migration-0079'),
        (mec_tenant, stationery_role, 'app', 'runtime-migration-0079')
    ON CONFLICT (tenant_id, role_id, surface) DO NOTHING;

    INSERT INTO authz.role_permissions
        (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by)
    SELECT mec_tenant, librarian_role, permission.permission_key, 'app',
           'all', '{}'::jsonb, 'runtime-migration-0079'
    FROM (VALUES
        ('library.visit_pass.read'),
        ('library.qr_pass.read'),
        ('library.visit_history.read'),
        ('library.occupancy.read'),
        ('library.visit_pass.approve')
    ) permission(permission_key)
    ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
        scope = EXCLUDED.scope, constraints = EXCLUDED.constraints,
        granted_by = EXCLUDED.granted_by, granted_at = now();

    INSERT INTO authz.role_permissions
        (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by)
    SELECT mec_tenant, stationery_role, permission.permission_key, 'app',
           'assigned', '{}'::jsonb, 'runtime-migration-0079'
    FROM (VALUES
        ('canteen.menu.read'), ('canteen.order.read'),
        ('canteen.orders.manage'), ('canteen.analytics.read')
    ) permission(permission_key)
    ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
        scope = EXCLUDED.scope, constraints = EXCLUDED.constraints,
        granted_by = EXCLUDED.granted_by, granted_at = now();

    INSERT INTO identity.users
        (email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('librarian@mec.local', password_hash, 'MEC Librarian', 'ML',
         'staff', true, '{"designation":"Librarian","team":"Library","dept":"Library"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
        password_hash = EXCLUDED.password_hash,
        display_name = EXCLUDED.display_name, initials = EXCLUDED.initials,
        account_type = 'staff', active = true, profile = EXCLUDED.profile,
        updated_at = now()
    RETURNING id INTO librarian_id;

    INSERT INTO identity.users
        (email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('stationary@mec.local', password_hash, 'MEC Stationery', 'MS',
         'staff', true, '{"designation":"Stationery Operator","team":"Stationery","dept":"Stationery"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
        password_hash = EXCLUDED.password_hash,
        display_name = EXCLUDED.display_name, initials = EXCLUDED.initials,
        account_type = 'staff', active = true, profile = EXCLUDED.profile,
        updated_at = now()
    RETURNING id INTO stationery_id;

    INSERT INTO identity.tenant_memberships
        (tenant_id, user_id, roles, active, is_primary, profile)
    VALUES
        (mec_tenant, librarian_id, ARRAY['librarian']::text[], true, true,
         '{"designation":"Librarian","team":"Library","dept":"Library"}'::jsonb),
        (mec_tenant, stationery_id, ARRAY['stationery_operator']::text[], true, true,
         '{"designation":"Stationery Operator","team":"Stationery","dept":"Stationery"}'::jsonb)
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        roles = EXCLUDED.roles, active = true, is_primary = true,
        profile = EXCLUDED.profile, updated_at = now();

    DELETE FROM authz.user_roles WHERE tenant_id = mec_tenant
      AND user_id = librarian_id AND role_id <> librarian_role;
    DELETE FROM authz.user_roles WHERE tenant_id = mec_tenant
      AND user_id = stationery_id AND role_id <> stationery_role;
    INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
    VALUES
        (mec_tenant, librarian_id, librarian_role, 'runtime-migration-0079'),
        (mec_tenant, stationery_id, stationery_role, 'runtime-migration-0079')
    ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;

    INSERT INTO core.employees
        (id, tenant_id, user_id, employee_number, full_name, email, status, profile)
    VALUES
        (librarian_id, mec_tenant, librarian_id, 'MECLIB001', 'MEC Librarian',
         'librarian@mec.local', 'active', '{"designation":"Librarian","team":"Library"}'::jsonb),
        (stationery_id, mec_tenant, stationery_id, 'MECSTA001', 'MEC Stationery',
         'stationary@mec.local', 'active', '{"designation":"Stationery Operator","team":"Stationery"}'::jsonb)
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        employee_number = EXCLUDED.employee_number, full_name = EXCLUDED.full_name,
        email = EXCLUDED.email, status = 'active', profile = EXCLUDED.profile,
        updated_at = now();

    UPDATE identity.auth_sessions SET revoked_at = COALESCE(revoked_at, now())
     WHERE tenant_id = mec_tenant AND user_id IN (librarian_id::text, stationery_id::text);
END $$;
