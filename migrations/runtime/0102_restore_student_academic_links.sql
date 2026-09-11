-- The legacy student editor saved display labels (for example "A") into the
-- canonical section_id column. Restore the exact academic links from each
-- student's active enrollment. Guardian rows are intentionally untouched.
WITH latest_enrollment AS (
    SELECT DISTINCT ON (enrollment.tenant_id, enrollment.student_id)
           enrollment.tenant_id,
           enrollment.student_id,
           enrollment.department_id,
           enrollment.section_id
    FROM core.academic_enrollments AS enrollment
    WHERE enrollment.section_id IS NOT NULL
      AND enrollment.status IN ('active', 'provisional')
    ORDER BY enrollment.tenant_id,
             enrollment.student_id,
             CASE enrollment.status WHEN 'active' THEN 0 ELSE 1 END,
             enrollment.started_at DESC,
             enrollment.created_at DESC
)
UPDATE core.students AS student
SET department_id = enrollment.department_id::text,
    section_id = enrollment.section_id::text,
    profile = COALESCE(student.profile, '{}'::jsonb)
        || jsonb_build_object(
             'departmentId', enrollment.department_id,
             'sectionId', enrollment.section_id
           ),
    updated_at = now()
FROM latest_enrollment AS enrollment,
     platform.tenants AS tenant
WHERE tenant.id = student.tenant_id
  AND tenant.slug = 'mec'
  AND enrollment.tenant_id = student.tenant_id
  AND enrollment.student_id = student.id
  AND (
       student.department_id IS NULL
       OR student.section_id IS NULL
       OR student.department_id !~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR student.section_id !~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR NOT EXISTS (
            SELECT 1
            FROM core.sections AS section
            WHERE section.tenant_id = student.tenant_id
              AND section.id::text = student.section_id
       )
  );
