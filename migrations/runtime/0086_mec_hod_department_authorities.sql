-- Connect every MEC HOD identity to the department it governs. Role grants
-- determine what an HOD may do; this table determines which department rows
-- the HOD may see and review.
DO $$
DECLARE
    mec_id uuid;
    assigner_id uuid;
BEGIN
    SELECT id INTO mec_id FROM platform.tenants WHERE slug = 'mec';
    IF mec_id IS NULL THEN
        RETURN;
    END IF;

    SELECT id INTO assigner_id
      FROM identity.users
     WHERE email = 'principal@mec.local'
     LIMIT 1;

    IF assigner_id IS NULL THEN
        RAISE EXCEPTION 'cannot assign MEC HOD departments without principal@mec.local';
    END IF;

    INSERT INTO core.department_authorities
        (tenant_id, department_id, user_id, authority_role, active, assigned_by)
    SELECT mec_id, department.id, hod.id, 'hod', true, assigner_id
      FROM (VALUES
        ('AIDS',  'hod.aids@mec.local'),
        ('AIML',  'hod.aiml@mec.local'),
        ('CSE',   'hod.cse@mec.local'),
        ('CYBER', 'hod.cyber@mec.local'),
        ('CSBS',  'hod.csbs@mec.local'),
        ('IT',    'hod.it@mec.local')
      ) requested(department_code, hod_email)
      JOIN core.departments department
        ON department.tenant_id = mec_id
       AND upper(department.code) = requested.department_code
      JOIN identity.users hod
        ON lower(hod.email) = requested.hod_email
    ON CONFLICT (tenant_id, department_id, user_id, authority_role)
    DO UPDATE SET
        active = true,
        assigned_by = EXCLUDED.assigned_by,
        updated_at = now();
END $$;
