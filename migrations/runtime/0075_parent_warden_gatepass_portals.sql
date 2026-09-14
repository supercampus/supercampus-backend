-- Account-backed parent and warden portals for the MEC outpass matrix.
--
-- Demo credentials deliberately follow the MEC seed convention. Production
-- operators must rotate them after the acceptance demo.

-- The previous schema kept only the QR hash. That made an issued QR impossible
-- to fetch again after the warden's response had completed. The opaque payload
-- contains no student data and is returned only through scoped endpoints.
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
        roles = ARRAY['parent']::text[], active = true, is_primary = true,
        profile = identity.tenant_memberships.profile || EXCLUDED.profile,
        updated_at = now();

    INSERT INTO identity.users
        (email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('warden@mec.local', crypt('Mec@2026', gen_salt('bf', 12)),
         'MEC Hostel Warden', 'MW', 'staff', true,
         '{"designation":"Hostel Warden","team":"Hostel"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
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
        roles = ARRAY['warden']::text[], active = true, is_primary = true,
        profile = identity.tenant_memberships.profile || EXCLUDED.profile,
        updated_at = now();

    -- The demo parent belongs to the Vishnu student shown throughout the MEC
    -- app references. Prefer the institutional number, then the exact name.
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
