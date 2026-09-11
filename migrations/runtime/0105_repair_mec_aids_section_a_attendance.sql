-- Guardian edits left three active AIDS Year 2 students visible to the class
-- advisor (department-scoped directory) but outside the Section A attendance
-- roster (section UUID-scoped). Build the repair from the independently known
-- 51-student cohort and fail loudly unless each affected 48-row snapshot has
-- exactly the expected three missing students.
CREATE TEMP TABLE attendance_repair_0105 ON COMMIT DROP AS
WITH target_sessions AS (
    SELECT session.tenant_id,
           session.id AS session_id,
           session.section_id,
           subject.department_id
    FROM campus_ops.attendance_sessions AS session
    JOIN platform.tenants AS tenant
      ON tenant.id = session.tenant_id
    JOIN core.subject_offerings AS offering
      ON offering.tenant_id = session.tenant_id
     AND offering.id = session.subject_offering_id
    JOIN core.subjects AS subject
      ON subject.tenant_id = offering.tenant_id
     AND subject.id = offering.subject_id
    JOIN core.departments AS department
      ON department.tenant_id = subject.tenant_id
     AND department.id = subject.department_id
    WHERE tenant.slug = 'mec'
      AND department.code = 'AIDS'
      AND session.held_on = DATE '2026-09-11'
      AND session.section_id IS NOT NULL
      AND session.status IN (
          'draft',
          'returned',
          'submitted_to_advisor',
          'submitted_to_hod',
          'submitted_to_principal',
          'approved'
      )
), candidate_students AS (
    SELECT target.tenant_id,
           target.session_id,
           target.section_id,
           student.id AS student_id,
           student.user_account_id,
           student.full_name
    FROM target_sessions AS target
    JOIN core.students AS student
      ON student.tenant_id = target.tenant_id
     AND student.department_id = target.department_id::text
    WHERE student.status = 'active'
      AND student.user_account_id IS NOT NULL
      AND lower(trim(COALESCE(
            NULLIF(student.profile ->> 'yearOfStudy', ''),
            NULLIF(student.profile ->> 'year', ''),
            NULLIF(student.academic_year, ''),
            ''
          ))) IN (
            '2', '2nd', '2nd year', 'ii', 'ii year', 'year ii',
            'year 2', 'second', 'second year'
          )
), validated_sessions AS (
    SELECT candidate.tenant_id,
           candidate.session_id
    FROM candidate_students AS candidate
    LEFT JOIN campus_ops.attendance_entries AS entry
      ON entry.tenant_id = candidate.tenant_id
     AND entry.session_id = candidate.session_id
     AND entry.student_user_id = candidate.user_account_id::text
    GROUP BY candidate.tenant_id, candidate.session_id
    HAVING count(*) = 51
       AND count(entry.student_user_id) = 48
       AND count(*) FILTER (WHERE entry.student_user_id IS NULL) = 3
)
SELECT candidate.tenant_id,
       candidate.session_id,
       candidate.section_id,
       candidate.student_id,
       candidate.user_account_id,
       candidate.full_name
FROM candidate_students AS candidate
JOIN validated_sessions AS valid
  ON valid.tenant_id = candidate.tenant_id
 AND valid.session_id = candidate.session_id
LEFT JOIN campus_ops.attendance_entries AS entry
  ON entry.tenant_id = candidate.tenant_id
 AND entry.session_id = candidate.session_id
 AND entry.student_user_id = candidate.user_account_id::text
WHERE entry.student_user_id IS NULL;

DO $$
DECLARE
    repair_sessions integer;
    repair_rows integer;
BEGIN
    SELECT count(DISTINCT session_id), count(*)
      INTO repair_sessions, repair_rows
      FROM attendance_repair_0105;

    IF repair_sessions < 5 OR repair_rows <> repair_sessions * 3 THEN
        RAISE EXCEPTION
          'Attendance repair safety check failed: expected at least 5 sessions and exactly 3 missing students per session; found % sessions and % rows',
          repair_sessions,
          repair_rows;
    END IF;
END
$$;

WITH restored_students AS (
    SELECT DISTINCT tenant_id, section_id, student_id
    FROM attendance_repair_0105
)
UPDATE core.students AS student
SET section_id = restored.section_id::text,
    profile = COALESCE(student.profile, '{}'::jsonb)
        || jsonb_build_object(
             'section', section.name,
             'sectionId', restored.section_id
           ),
    updated_at = now()
FROM restored_students AS restored
JOIN core.sections AS section
  ON section.tenant_id = restored.tenant_id
 AND section.id = restored.section_id
WHERE student.tenant_id = restored.tenant_id
  AND student.id = restored.student_id;

WITH restored_students AS (
    SELECT DISTINCT tenant_id, section_id, student_id
    FROM attendance_repair_0105
)
UPDATE core.academic_enrollments AS enrollment
SET section_id = restored.section_id,
    updated_at = now()
FROM restored_students AS restored
WHERE enrollment.tenant_id = restored.tenant_id
  AND enrollment.student_id = restored.student_id
  AND enrollment.status IN ('active', 'provisional');

INSERT INTO campus_ops.attendance_entries (
    tenant_id,
    session_id,
    student_user_id,
    student_name,
    status,
    marked_by
)
SELECT repair.tenant_id,
       repair.session_id,
       repair.user_account_id::text,
       repair.full_name,
       'present',
       session.faculty_user_id
FROM attendance_repair_0105 AS repair
JOIN campus_ops.attendance_sessions AS session
  ON session.tenant_id = repair.tenant_id
 AND session.id = repair.session_id
ON CONFLICT (tenant_id, session_id, student_user_id) DO NOTHING;
