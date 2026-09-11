-- Three MEC students were temporarily omitted from attendance snapshots after
-- guardian edits replaced their canonical section links with display labels.
-- Migration 0102 restored those links and recorded the canonical sectionId in
-- the profile. Repair every attendance session held on the affected day, not
-- only one subject, without changing existing marks or approval history.
INSERT INTO campus_ops.attendance_entries (
    tenant_id,
    session_id,
    student_user_id,
    student_name,
    status,
    marked_by
)
SELECT session.tenant_id,
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
WHERE tenant.slug = 'mec'
  AND session.held_on = DATE '2026-09-11'
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
  AND EXISTS (
      SELECT 1
      FROM core.academic_enrollments AS enrollment
      WHERE enrollment.tenant_id = student.tenant_id
        AND enrollment.student_id = student.id
        AND enrollment.section_id = session.section_id
        AND enrollment.status IN ('active', 'provisional')
  )
  AND NOT EXISTS (
      SELECT 1
      FROM campus_ops.attendance_entries AS existing
      WHERE existing.tenant_id = session.tenant_id
        AND existing.session_id = session.id
        AND existing.student_user_id = student.user_account_id::text
  )
ON CONFLICT (tenant_id, session_id, student_user_id) DO NOTHING;
