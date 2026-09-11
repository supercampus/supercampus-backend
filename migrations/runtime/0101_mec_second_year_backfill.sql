-- Legacy MEC students shown under "Year not assigned" are the existing
-- second-year cohort. Store the year canonically so every portal groups them
-- consistently and future edits preserve the assignment.
UPDATE core.students AS student
SET academic_year = 'Year 2',
    profile = COALESCE(student.profile, '{}'::jsonb)
        || jsonb_build_object('year', 'Year 2', 'yearOfStudy', 2),
    updated_at = now()
FROM platform.tenants AS tenant
WHERE tenant.id = student.tenant_id
  AND tenant.slug = 'mec'
  AND lower(trim(COALESCE(
        NULLIF(student.profile ->> 'yearOfStudy', ''),
        NULLIF(student.profile ->> 'year', ''),
        NULLIF(student.academic_year, ''),
        ''
      ))) IN (
        '',
        'unassigned',
        'not assigned',
        'year not assigned',
        'ii',
        'ii year',
        'year ii',
        '2',
        '2nd',
        '2nd year',
        'year 2',
        'second',
        'second year'
      );
