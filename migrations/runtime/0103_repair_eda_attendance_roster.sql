-- The 11 Sep 2026 EDA roll was submitted while three students had corrupted
-- section links. Migration 0102 restored those links and tagged their profile
-- with the canonical sectionId. Add only those restored students that are
-- absent from this saved session; existing marks and reviews remain unchanged.
INSERT INTO campus_ops.attendance_entries (
    tenant_id,
    session_id,
    student_user_id,
    student_name,
    status,
    marked_by
)
SELECT DISTINCT session.tenant_id,
       session.id,
       student.user_account_id::text,
       student.full_name,
       'present',
       session.faculty_user_id
FROM campus_ops.attendance_sessions AS session
JOIN platform.tenants AS tenant
  ON tenant.id = session.tenant_id
JOIN core.students AS student
  ON student.tenant_id = session.tenant_id
 AND student.section_id = session.section_id::text
JOIN core.academic_enrollments AS enrollment
  ON enrollment.tenant_id = student.tenant_id
 AND enrollment.student_id = student.id
 AND enrollment.section_id = session.section_id
 AND enrollment.status IN ('active', 'provisional')
WHERE tenant.slug = 'mec'
  AND session.held_on = DATE '2026-09-11'
  AND lower(session.subject_name) = 'exploratory data analysis'
  AND session.status IN (
      'draft',
      'returned',
      'submitted_to_advisor',
      'submitted_to_hod',
      'submitted_to_principal',
      'approved'
  )
  AND student.status = 'active'
  AND student.user_account_id IS NOT NULL
  AND student.profile ->> 'sectionId' = student.section_id
  AND NOT EXISTS (
      SELECT 1
      FROM campus_ops.attendance_entries AS existing
      WHERE existing.tenant_id = session.tenant_id
        AND existing.session_id = session.id
        AND existing.student_user_id = student.user_account_id::text
  )
ON CONFLICT (tenant_id, session_id, student_user_id) DO NOTHING;
