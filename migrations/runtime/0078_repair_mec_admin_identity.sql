-- Restore MEC account display identities from the tenant's authoritative
-- Student, Employee and Guardian master records. The legacy user-create upsert
-- could overwrite an existing identity with another person's profile.
DO $$
DECLARE
    mec_id uuid;
    admin_id uuid;
BEGIN
    SELECT id INTO mec_id
    FROM platform.tenants
    WHERE slug = 'mec';

    IF mec_id IS NULL THEN
        RETURN;
    END IF;

    UPDATE identity.users user_account
    SET display_name = student.full_name,
        initials = upper(concat(
            left(split_part(student.full_name, ' ', 1), 1),
            left(split_part(student.full_name, ' ', 2), 1)
        )),
        account_type = 'student',
        profile = COALESCE(student.profile, '{}'::jsonb)
            || jsonb_build_object(
                'roll', student.student_number,
                'team', 'Students',
                'dept', COALESCE(department.code, NULLIF(student.department_id, ''), '')
            ),
        updated_at = now()
    FROM core.students student
    LEFT JOIN core.departments department
      ON department.tenant_id = student.tenant_id
     AND department.id::text = student.department_id
    WHERE student.tenant_id = mec_id
      AND student.user_account_id = user_account.id
      AND student.status IN ('provisional', 'active');

    UPDATE identity.tenant_memberships membership
    SET profile = COALESCE(student.profile, '{}'::jsonb)
            || jsonb_build_object(
                'roll', student.student_number,
                'team', 'Students',
                'dept', COALESCE(department.code, NULLIF(student.department_id, ''), '')
            ),
        updated_at = now()
    FROM core.students student
    LEFT JOIN core.departments department
      ON department.tenant_id = student.tenant_id
     AND department.id::text = student.department_id
    WHERE membership.tenant_id = mec_id
      AND membership.user_id = student.user_account_id
      AND student.tenant_id = mec_id
      AND student.status IN ('provisional', 'active');

    UPDATE identity.users user_account
    SET display_name = employee.full_name,
        initials = upper(concat(
            left(split_part(employee.full_name, ' ', 1), 1),
            left(split_part(employee.full_name, ' ', 2), 1)
        )),
        account_type = 'staff',
        profile = (COALESCE(employee.profile, '{}'::jsonb)
            - 'roll' - 'year' - 'section' - 'residency' - 'gender')
            || jsonb_build_object('dept', COALESCE(department.code, '')),
        updated_at = now()
    FROM core.employees employee
    LEFT JOIN core.departments department
      ON department.tenant_id = employee.tenant_id
     AND department.id = employee.department_id
    WHERE employee.tenant_id = mec_id
      AND employee.user_id = user_account.id
      AND employee.status IN ('provisional', 'active');

    UPDATE identity.tenant_memberships membership
    SET profile = (COALESCE(employee.profile, '{}'::jsonb)
            - 'roll' - 'year' - 'section' - 'residency' - 'gender')
            || jsonb_build_object('dept', COALESCE(department.code, '')),
        updated_at = now()
    FROM core.employees employee
    LEFT JOIN core.departments department
      ON department.tenant_id = employee.tenant_id
     AND department.id = employee.department_id
    WHERE membership.tenant_id = mec_id
      AND membership.user_id = employee.user_id
      AND employee.tenant_id = mec_id
      AND employee.status IN ('provisional', 'active');

    UPDATE identity.users user_account
    SET display_name = guardian.full_name,
        initials = upper(concat(
            left(split_part(guardian.full_name, ' ', 1), 1),
            left(split_part(guardian.full_name, ' ', 2), 1)
        )),
        profile = COALESCE(guardian.profile, '{}'::jsonb),
        updated_at = now()
    FROM core.guardians guardian
    WHERE guardian.tenant_id = mec_id
      AND guardian.user_id = user_account.id;

    UPDATE identity.tenant_memberships membership
    SET profile = COALESCE(guardian.profile, '{}'::jsonb),
        updated_at = now()
    FROM core.guardians guardian
    WHERE membership.tenant_id = mec_id
      AND membership.user_id = guardian.user_id
      AND guardian.tenant_id = mec_id;

    SELECT id INTO admin_id
    FROM identity.users
    WHERE email = 'admin@mec.local';

    IF admin_id IS NOT NULL THEN

    UPDATE identity.users
    SET display_name = 'Arun Iyer',
        initials = 'AI',
        account_type = 'staff',
        profile = (COALESCE(profile, '{}'::jsonb)
            - 'roll' - 'year' - 'section' - 'residency' - 'gender' - 'photoUrl')
            || '{"designation":"Tenant Administrator","team":"Administration","dept":""}'::jsonb,
        active = true,
        updated_at = now()
    WHERE id = admin_id;

    UPDATE identity.tenant_memberships
    SET roles = ARRAY['tenant_admin']::text[],
        profile = '{"designation":"Tenant Administrator","team":"Administration","dept":""}'::jsonb,
        active = true,
        updated_at = now()
    WHERE tenant_id = mec_id
      AND user_id = admin_id;

    DELETE FROM authz.user_roles
    WHERE tenant_id = mec_id
      AND user_id = admin_id;

    INSERT INTO authz.user_roles
        (tenant_id, user_id, role_id, assigned_by)
    SELECT mec_id, admin_id, role.id, 'runtime-migration-0078'
    FROM authz.roles role
    WHERE role.tenant_id = mec_id
      AND role.role_key = 'tenant_admin'
      AND role.active
    ON CONFLICT (tenant_id, user_id, role_id) DO UPDATE SET
        assigned_by = EXCLUDED.assigned_by,
        assigned_at = now();

    UPDATE core.employees
    SET full_name = 'Arun Iyer',
        email = 'admin@mec.local',
        status = 'active',
        profile = '{"designation":"Tenant Administrator","team":"Administration","dept":""}'::jsonb,
        updated_at = now()
    WHERE tenant_id = mec_id
      AND user_id = admin_id;

    -- A staff identity must never remain attached to a student master row.
    UPDATE core.students
    SET user_account_id = NULL,
        updated_at = now()
    WHERE tenant_id = mec_id
      AND user_account_id = admin_id;

    END IF;

    -- Session rows contain a profile snapshot. Revoke every MEC snapshot once
    -- so the next login receives its own freshly hydrated master identity.
    UPDATE identity.auth_sessions
    SET revoked_at = now()
    WHERE tenant_id = mec_id
      AND revoked_at IS NULL;
END
$$;
