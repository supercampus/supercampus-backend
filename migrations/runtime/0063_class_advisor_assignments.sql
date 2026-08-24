-- A class advisor may own more than one department (for example one member of
-- staff may advise both CSE and Cyber).  This is deliberately separate from
-- department_authorities: that table models HOD authority and enforces its
-- own role semantics.
CREATE TABLE IF NOT EXISTS core.class_advisor_assignments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    department_id uuid NOT NULL,
    advisor_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    active boolean NOT NULL DEFAULT true,
    assigned_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE CASCADE,
    UNIQUE (tenant_id, department_id),
    UNIQUE (tenant_id, department_id, advisor_user_id)
);

CREATE INDEX IF NOT EXISTS class_advisor_assignments_user_idx
    ON core.class_advisor_assignments (tenant_id, advisor_user_id, active);


-- Apply the requested MEC advisor roster without touching passwords or student
-- data. On the control plane this updates identities and memberships; on the
-- MEC tenant database it also updates employees and creates the assignments.
DO $$
DECLARE
    mec_id uuid;
    principal_id uuid;
BEGIN
    SELECT id INTO mec_id FROM platform.tenants WHERE slug = 'mec';
    IF mec_id IS NULL THEN
        RETURN;
    END IF;

    SELECT id INTO principal_id FROM identity.users
    WHERE email = 'principal@mec.local';

    UPDATE identity.users person
    SET display_name = requested.name,
        initials = requested.initials,
        profile = jsonb_build_object(
            'designation', 'Class Advisor, ' || requested.departments,
            'team', 'Academics',
            'dept', requested.departments
        ),
        active = true,
        updated_at = now()
    FROM (VALUES
        ('advisor.aids@mec.local', 'Shobana', 'S', 'AIDS'),
        ('advisor.csbs@mec.local', 'Elakkiya', 'E', 'CSBS'),
        ('advisor.it@mec.local', 'Devnath', 'D', 'IT'),
        ('advisor.cse@mec.local', 'Hari Rama Krishna', 'HRK', 'CSE & CYBER'),
        ('advisor.aiml@mec.local', 'Karthikeyan', 'K', 'AIML')
    ) requested(email, name, initials, departments)
    WHERE person.email = requested.email;

    UPDATE identity.tenant_memberships membership
    SET roles = ARRAY['staff', 'class_advisor']::text[],
        profile = person.profile,
        active = true,
        updated_at = now()
    FROM identity.users person
    WHERE membership.tenant_id = mec_id
      AND membership.user_id = person.id
      AND person.email IN (
          'advisor.aids@mec.local',
          'advisor.csbs@mec.local',
          'advisor.it@mec.local',
          'advisor.cse@mec.local',
          'advisor.aiml@mec.local'
      );

    UPDATE core.employees employee
    SET full_name = person.display_name,
        profile = person.profile,
        status = 'active',
        updated_at = now()
    FROM identity.users person
    WHERE employee.tenant_id = mec_id
      AND employee.user_id = person.id
      AND person.email IN (
          'advisor.aids@mec.local',
          'advisor.csbs@mec.local',
          'advisor.it@mec.local',
          'advisor.cse@mec.local',
          'advisor.aiml@mec.local'
      );

    IF principal_id IS NOT NULL THEN
        INSERT INTO core.class_advisor_assignments
            (tenant_id, department_id, advisor_user_id, active, assigned_by)
        SELECT mec_id, department.id, advisor.id, true, principal_id
        FROM (VALUES
            ('AIDS', 'advisor.aids@mec.local'),
            ('CSBS', 'advisor.csbs@mec.local'),
            ('IT', 'advisor.it@mec.local'),
            ('CSE', 'advisor.cse@mec.local'),
            ('CYBER', 'advisor.cse@mec.local'),
            ('AIML', 'advisor.aiml@mec.local')
        ) requested(department_code, advisor_email)
        JOIN core.departments department
          ON department.tenant_id = mec_id
         AND department.code = requested.department_code
        JOIN identity.users advisor ON advisor.email = requested.advisor_email
        ON CONFLICT (tenant_id, department_id) DO UPDATE SET
            advisor_user_id = EXCLUDED.advisor_user_id,
            active = true,
            updated_at = now();
    END IF;

    UPDATE identity.tenant_memberships membership
    SET active = false, updated_at = now()
    FROM identity.users person
    WHERE membership.tenant_id = mec_id
      AND membership.user_id = person.id
      AND person.email = 'advisor.cyber@mec.local';

    UPDATE core.employees
    SET status = 'inactive', updated_at = now()
    WHERE tenant_id = mec_id AND email = 'advisor.cyber@mec.local';

    UPDATE identity.users
    SET active = false, updated_at = now()
    WHERE email = 'advisor.cyber@mec.local';
END
$$;
