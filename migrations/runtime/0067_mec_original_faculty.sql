-- Replace MEC's generated academic identities with the faculty named in the
-- institution's 2026-27 odd-semester timetable. Existing identity UUIDs are
-- retained wherever possible so timetable, attendance, and audit references
-- remain valid. The three additional faculty receive deterministic UUIDs.
DO $$
DECLARE
    mec_id uuid;
    principal_hash text;
    requested record;
    existing_id uuid;
    conflicting_id uuid;
BEGIN
    SELECT id INTO mec_id FROM platform.tenants WHERE slug = 'mec';
    IF mec_id IS NULL THEN
        RETURN;
    END IF;

    FOR requested IN
        SELECT * FROM (VALUES
            ('advisor.aids@mec.local', 'shobana@mec.local', 'Dr. G. Shobana', 'GS', 'AIDS', ARRAY['staff','class_advisor']::text[], 'Class Advisor, AIDS'),
            ('advisor.csbs@mec.local', 'elakkiya@mec.local', 'Mrs. J. Elakkiya', 'JE', 'CSBS', ARRAY['staff','class_advisor']::text[], 'Class Advisor, CSBS'),
            ('advisor.it@mec.local', 'devanath@mec.local', 'Dr. J. Devanath', 'JD', 'IT', ARRAY['staff','class_advisor']::text[], 'Class Advisor, IT'),
            ('advisor.cse@mec.local', 'hariramakrishna@mec.local', 'Mr. S. Hari Rama Krishna', 'SH', 'CSE & CYBER', ARRAY['staff','class_advisor']::text[], 'Class Advisor, CSE & CYBER'),
            ('advisor.aiml@mec.local', 'karthikeyan@mec.local', 'Dr. M. Karthikeyan', 'MK', 'AIML', ARRAY['staff','class_advisor']::text[], 'Class Advisor, AIML'),
            ('faculty01@mec.local', 'saranya@mec.local', 'Dr. N. Saranya', 'NS', 'IT', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty02@mec.local', 'anitha@mec.local', 'Dr. S. M. Anitha', 'SA', 'CSBS', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty03@mec.local', 'saranyaa@mec.local', 'Dr. P. Saranyaa', 'PS', 'AIML', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty04@mec.local', 'deepika@mec.local', 'Dr. T. Deepika', 'TD', 'CSE', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty05@mec.local', 'lakshmikanth@mec.local', 'Mr. J. Lakshmikanth', 'JL', 'AIDS', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty06@mec.local', 'vijayakumar@mec.local', 'Mr. K. Vijayakumar', 'KV', 'CSE', ARRAY['staff']::text[], 'Assistant Professor'),
            ('faculty07@mec.local', 'arunmozhi@mec.local', 'Mr. K. Arunmozhi', 'KA', 'IT', ARRAY['staff']::text[], 'Assistant Professor')
        ) AS roster(old_email, new_email, full_name, initials, department, roles, designation)
    LOOP
        SELECT id INTO existing_id FROM identity.users WHERE email = requested.old_email;
        SELECT id INTO conflicting_id FROM identity.users WHERE email = requested.new_email;

        IF existing_id IS NOT NULL AND conflicting_id IS NOT NULL AND existing_id <> conflicting_id THEN
            RAISE EXCEPTION 'cannot replace %, target identity % already exists', requested.old_email, requested.new_email;
        END IF;

        UPDATE identity.users
        SET email = requested.new_email,
            display_name = requested.full_name,
            initials = requested.initials,
            profile = jsonb_build_object(
                'designation', requested.designation,
                'team', 'Academics',
                'dept', requested.department
            ),
            active = true,
            updated_at = now()
        WHERE id = COALESCE(existing_id, conflicting_id);

        UPDATE identity.tenant_memberships
        SET roles = requested.roles,
            profile = jsonb_build_object(
                'designation', requested.designation,
                'team', 'Academics',
                'dept', requested.department
            ),
            active = true,
            updated_at = now()
        WHERE tenant_id = mec_id
          AND user_id = COALESCE(existing_id, conflicting_id);

        UPDATE core.employees
        SET full_name = requested.full_name,
            email = requested.new_email,
            profile = jsonb_build_object(
                'designation', requested.designation,
                'team', 'Academics',
                'dept', requested.department
            ),
            status = 'active',
            updated_at = now()
        WHERE tenant_id = mec_id
          AND user_id = COALESCE(existing_id, conflicting_id);
    END LOOP;

    SELECT password_hash INTO principal_hash
    FROM identity.users
    WHERE email = 'principal@mec.local';

    IF principal_hash IS NULL THEN
        RAISE EXCEPTION 'principal identity is required before adding MEC faculty';
    END IF;

    INSERT INTO identity.users
        (id, email, password_hash, display_name, initials, account_type, active, profile)
    VALUES
        ('696c2b7e-c50f-5fd6-9960-0bc1baec95d5'::uuid, 'ganesh@mec.local', principal_hash, 'Mr. S. Ganesh', 'SG', 'staff', true, '{"designation":"Assistant Professor","team":"Academics","dept":"CSBS"}'::jsonb),
        ('d09add63-dfa7-58f4-9d21-cedfd49fd635'::uuid, 'santhosh@mec.local', principal_hash, 'Mr. S. Santhosh', 'SS', 'staff', true, '{"designation":"Assistant Professor","team":"Academics","dept":"AIDS"}'::jsonb),
        ('0382a265-e274-57d5-8d40-6aa7ee287e09'::uuid, 'preethi@mec.local', principal_hash, 'Mrs. Preethi', 'P', 'staff', true, '{"designation":"Assistant Professor","team":"Academics","dept":"CSE"}'::jsonb)
    ON CONFLICT (email) DO UPDATE SET
        display_name = EXCLUDED.display_name,
        initials = EXCLUDED.initials,
        account_type = EXCLUDED.account_type,
        active = true,
        profile = EXCLUDED.profile,
        updated_at = now();

    INSERT INTO identity.tenant_memberships
        (tenant_id, user_id, roles, active, is_primary, profile)
    SELECT mec_id, person.id, ARRAY['staff']::text[], true, true, person.profile
    FROM identity.users person
    WHERE person.email IN ('ganesh@mec.local', 'santhosh@mec.local', 'preethi@mec.local')
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        roles = EXCLUDED.roles,
        active = true,
        profile = EXCLUDED.profile,
        updated_at = now();

    INSERT INTO core.employees
        (id, tenant_id, user_id, employee_number, department_id, full_name, email, status, profile)
    SELECT addition.employee_id, mec_id, person.id, addition.employee_number,
           department.id, person.display_name, person.email, 'active', person.profile
    FROM (VALUES
        ('202c91e9-fe3c-5659-998f-b2a9811c9668'::uuid, 'MECEMP036', 'ganesh@mec.local', 'CSBS'),
        ('ac70c364-2a32-51b5-98f4-0882b3844a84'::uuid, 'MECEMP037', 'santhosh@mec.local', 'AIDS'),
        ('170ce611-48f1-5983-9c9a-5ae85947a754'::uuid, 'MECEMP038', 'preethi@mec.local', 'CSE')
    ) addition(employee_id, employee_number, email, department_code)
    JOIN identity.users person ON person.email = addition.email
    JOIN core.departments department
      ON department.tenant_id = mec_id
     AND department.code = addition.department_code
    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
        department_id = EXCLUDED.department_id,
        full_name = EXCLUDED.full_name,
        email = EXCLUDED.email,
        status = 'active',
        profile = EXCLUDED.profile,
        updated_at = now();
END
$$;

