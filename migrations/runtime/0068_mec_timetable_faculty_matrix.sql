-- Correct the MEC AI&DS English lab allocation to the faculty matrix supplied
-- by the college. Deepika teaches EN25C03 for CSE/CYBER/IT; the AI&DS matrix
-- assigns that course to Santhosh.
DO $$
DECLARE
    mec_tenant uuid;
    deepika_user uuid;
    santhosh_user uuid;
    aids_offering uuid;
    santhosh_assignment uuid;
BEGIN
    SELECT id INTO mec_tenant
    FROM platform.tenants
    WHERE slug = 'mec'
    LIMIT 1;

    IF mec_tenant IS NULL THEN
        RETURN;
    END IF;

    SELECT id INTO deepika_user
    FROM identity.users
    WHERE lower(email) = 'deepika@mec.local'
    LIMIT 1;

    SELECT id INTO santhosh_user
    FROM identity.users
    WHERE lower(email) = 'santhosh@mec.local'
    LIMIT 1;

    SELECT offering.id INTO aids_offering
    FROM core.subject_offerings offering
    JOIN core.subjects subject
      ON subject.tenant_id = offering.tenant_id
     AND subject.id = offering.subject_id
    JOIN core.sections section
      ON section.tenant_id = offering.tenant_id
     AND section.id = offering.section_id
    JOIN core.batches batch ON batch.id = section.batch_id
    JOIN core.programmes programme ON programme.id = batch.programme_id
    JOIN core.departments department ON department.id = programme.department_id
    WHERE offering.tenant_id = mec_tenant
      AND upper(subject.code) = 'EN25C03'
      AND upper(department.code) IN ('AIDS', 'AI&DS')
      AND offering.active
    ORDER BY offering.created_at DESC
    LIMIT 1;

    IF deepika_user IS NULL OR santhosh_user IS NULL OR aids_offering IS NULL THEN
        RETURN;
    END IF;

    SELECT id INTO santhosh_assignment
    FROM core.teaching_assignments
    WHERE tenant_id = mec_tenant
      AND subject_offering_id = aids_offering
      AND faculty_user_id = santhosh_user
      AND assignment_type = 'primary'
    LIMIT 1;

    IF santhosh_assignment IS NULL THEN
        INSERT INTO core.teaching_assignments
            (tenant_id, subject_offering_id, faculty_user_id, assignment_type,
             active, assigned_by, metadata)
        VALUES
            (mec_tenant, aids_offering, santhosh_user, 'primary', true,
             santhosh_user, '{"source":"mec_official_timetable_matrix"}'::jsonb)
        RETURNING id INTO santhosh_assignment;
    ELSE
        UPDATE core.teaching_assignments
        SET active = true,
            metadata = metadata || '{"source":"mec_official_timetable_matrix"}'::jsonb,
            updated_at = now()
        WHERE id = santhosh_assignment;
    END IF;

    UPDATE core.timetable_entries entry
    SET teaching_assignment_id = santhosh_assignment,
        updated_at = now(),
        metadata = entry.metadata || '{"facultyMatrixCorrected":true}'::jsonb
    FROM core.teaching_assignments prior
    WHERE entry.tenant_id = mec_tenant
      AND entry.subject_offering_id = aids_offering
      AND prior.id = entry.teaching_assignment_id
      AND prior.faculty_user_id = deepika_user;

    UPDATE core.teaching_assignments
    SET active = false,
        updated_at = now(),
        metadata = metadata || '{"replacedByFacultyMatrix":true}'::jsonb
    WHERE tenant_id = mec_tenant
      AND subject_offering_id = aids_offering
      AND faculty_user_id = deepika_user;
END $$;
