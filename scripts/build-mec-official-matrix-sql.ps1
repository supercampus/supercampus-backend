param(
    [Parameter(Mandatory = $true)]
    [string]$MatrixPath
)

$ErrorActionPreference = 'Stop'

function SqlText([AllowNull()][string]$Value) {
    if ($null -eq $Value) { return 'NULL' }
    return "'" + $Value.Replace("'", "''") + "'"
}

function NameKey([string]$Value) {
    return (($Value.ToLowerInvariant()) -replace '[^a-z]', '')
}

$matrix = Get-Content -LiteralPath $MatrixPath -Raw | ConvertFrom-Json
$facultyEmails = @{
    'drjdevanath' = 'devanath@mec.local'
    'mrjlakshmikanth' = 'lakshmikanth@mec.local'
    'mrspreethi' = 'preethi@mec.local'
    'mrkarunmozhi' = 'arunmozhi@mec.local'
    'mrkvijayakumar' = 'vijayakumar@mec.local'
    'drtdeepika' = 'deepika@mec.local'
    'drpsaranyaa' = 'saranyaa@mec.local'
    'mrsjelakkiya' = 'elakkiya@mec.local'
    'drgshobana' = 'shobana@mec.local'
    'mrssanthosh' = 'santhosh@mec.local'
    'drmkarthikeyan' = 'karthikeyan@mec.local'
    'drnsaranya' = 'saranya@mec.local'
    'mrshariramakrishnan' = 'hariramakrishna@mec.local'
    'drsmanitha' = 'anitha@mec.local'
    'mrsganesh' = 'ganesh@mec.local'
}
$advisorEmails = @{
    'CSE' = 'vijayakumar@mec.local'
    'CYBER' = 'vijayakumar@mec.local'
    'AIML' = 'elakkiya@mec.local'
    'AIDS' = 'shobana@mec.local'
    'IT' = 'preethi@mec.local'
    'CSBS' = 'elakkiya@mec.local'
}
$departmentCodes = @{
    'II/CSE' = 'CSE'
    'II/CS' = 'CYBER'
    'II/AIML' = 'AIML'
    'II/AI&DS' = 'AIDS'
    'II/IT' = 'IT'
    'II/CSBS' = 'CSBS'
}
$dayNumbers = @{ Monday = 1; Tuesday = 2; Wednesday = 3; Thursday = 4; Friday = 5 }
$periodNumbers = @{ I = 1; II = 2; III = 3; IV = 4; V = 5; VI = 6; VII = 7 }

$courseRows = [System.Collections.Generic.List[string]]::new()
$slotRows = [System.Collections.Generic.List[string]]::new()

foreach ($department in $matrix.departments) {
    $departmentCode = $departmentCodes[$department.year_branch]
    if (-not $departmentCode) { throw "Unsupported department $($department.year_branch)" }

    $aliases = @{}
    foreach ($course in $department.courses) {
        $code = [string]$course.course_code
        if ([string]::IsNullOrWhiteSpace($code)) {
            if ($course.course_name -match '^Skill Development') { $code = 'SD-I' }
            elseif ($course.course_name -match '^Library') { $code = 'LIB' }
            else { throw "Missing course code for $($course.course_name)" }
        }

        if ($course.course_name -match '\(([^()]*)\)\s*$') {
            $alias = $Matches[1].Trim()
        } elseif ($course.course_name -match '^Library') {
            $alias = 'Lib'
        } elseif ($course.course_name -match '^Skill Development') {
            $alias = 'SD-I'
        } else {
            throw "Cannot derive course alias from $($course.course_name)"
        }
        $alias = ($alias -replace '^Club Activities$', 'SD-I')
        $aliases[$alias.ToUpperInvariant()] = $code

        $facultyKey = NameKey ([string]$course.faculty_name)
        $facultyEmail = if ($facultyKey -eq 'concernfacultyincharge') {
            $advisorEmails[$departmentCode]
        } else {
            $facultyEmails[$facultyKey]
        }
        if (-not $facultyEmail) { throw "No faculty login mapping for $($course.faculty_name)" }

        $courseRows.Add("($(SqlText $departmentCode),$(SqlText $code),$(SqlText ([string]$course.course_name)),$(SqlText ([string]$course.course_type)),$(SqlText $facultyEmail),$([int]$course.work_load))")
    }

    foreach ($dayProperty in $department.timetable.PSObject.Properties) {
        $dayNumber = $dayNumbers[$dayProperty.Name]
        foreach ($slot in $dayProperty.Value) {
            $activity = ([string]$slot.activity).Trim()
            $baseAlias = ($activity -replace '\s*-?\s*Lab$', '' -replace '\(T\)$', '').Trim()
            if ($activity -eq 'CA') { $baseAlias = 'SD-I' }
            if ($activity -eq 'Lib') { $baseAlias = 'Lib' }
            $code = $aliases[$baseAlias.ToUpperInvariant()]
            if (-not $code) { throw "No course for $departmentCode activity $activity" }
            $deliveryType = if ($activity -match 'Lab$') { 'laboratory' } elseif ($activity -match '\(T\)$') { 'tutorial' } elseif ($activity -in @('CA','Lib')) { 'activity' } else { 'class' }
            $slotRows.Add("($(SqlText $departmentCode),$dayNumber,$($periodNumbers[[string]$slot.period]),$(SqlText $activity),$(SqlText $code),$(SqlText $deliveryType))")
        }
    }
}

$courseValues = $courseRows -join ",`n        "
$slotValues = $slotRows -join ",`n        "

$sql = @"
BEGIN;
CREATE TEMP TABLE matrix_courses (
    department_code text, subject_code text, subject_name text, course_type text,
    faculty_email text, workload smallint
) ON COMMIT DROP;
INSERT INTO matrix_courses VALUES
        $courseValues;

CREATE TEMP TABLE matrix_slots (
    department_code text, day_no smallint, period_no smallint, activity text,
    subject_code text, delivery_type text
) ON COMMIT DROP;
INSERT INTO matrix_slots VALUES
        $slotValues;

DO `$matrix_apply`$
DECLARE
    mec_tenant uuid;
    principal_id uuid;
    config_id uuid;
    year_id uuid;
    term_value uuid;
    official_version uuid := 'ad262027-0000-4000-8000-000000000001'::uuid;
BEGIN
    SELECT id INTO mec_tenant FROM platform.tenants WHERE slug = 'mec';
    SELECT id INTO principal_id FROM identity.users WHERE lower(email) = 'principal@mec.local';
    SELECT id, academic_year_id, term_id INTO config_id, year_id, term_value
      FROM core.timetable_configurations
     WHERE tenant_id = mec_tenant AND name = 'MEC Semester Timetable 2026-27'
     ORDER BY created_at DESC LIMIT 1;
    IF mec_tenant IS NULL OR principal_id IS NULL OR config_id IS NULL THEN
        RAISE EXCEPTION 'MEC tenant, principal or timetable configuration is missing';
    END IF;

    IF EXISTS (
        SELECT 1 FROM matrix_courses course
        LEFT JOIN identity.users person ON lower(person.email) = lower(course.faculty_email)
        WHERE person.id IS NULL
    ) THEN
        RAISE EXCEPTION 'one or more official faculty identities are missing';
    END IF;

    CREATE TEMP TABLE matrix_sections ON COMMIT DROP AS
    SELECT wanted.department_code, section.id AS section_id, department.id AS department_id,
           room.id AS room_id
      FROM (SELECT DISTINCT department_code FROM matrix_courses) wanted
      JOIN core.departments department
        ON department.tenant_id = mec_tenant AND upper(department.code) = wanted.department_code
      JOIN core.programmes programme ON programme.department_id = department.id
      JOIN core.batches batch ON batch.programme_id = programme.id
      JOIN core.sections section ON section.batch_id = batch.id
      JOIN core.rooms room ON room.tenant_id = mec_tenant
                          AND room.department_id = department.id
                          AND room.active
     WHERE section.tenant_id = mec_tenant
       AND upper(section.name) LIKE wanted.department_code || '%SECTION A'
       AND room.code = wanted.department_code || '-CR';

    IF (SELECT count(*) FROM matrix_sections) <> 6 THEN
        RAISE EXCEPTION 'expected six official MEC sections and department rooms';
    END IF;

    INSERT INTO core.subjects
        (tenant_id, department_id, code, name, credits, active, metadata)
    SELECT mec_tenant, min(section.department_id::text)::uuid, course.subject_code,
           min(course.subject_name), NULL, true,
           jsonb_build_object('source','MEC official odd semester timetable 2026-27')
      FROM matrix_courses course
      JOIN matrix_sections section USING (department_code)
     GROUP BY course.subject_code
    ON CONFLICT (tenant_id, code) DO UPDATE SET
        name = EXCLUDED.name, active = true,
        metadata = core.subjects.metadata || EXCLUDED.metadata,
        updated_at = now();

    INSERT INTO core.subject_offerings
        (tenant_id, subject_id, academic_year_id, term_id, section_id, active, metadata)
    SELECT mec_tenant, subject.id, year_id, term_value, section.section_id, true,
           jsonb_build_object('source','MEC official odd semester timetable 2026-27',
                              'departmentCode',course.department_code)
      FROM matrix_courses course
      JOIN matrix_sections section USING (department_code)
      JOIN core.subjects subject
        ON subject.tenant_id = mec_tenant AND subject.code = course.subject_code
     WHERE NOT EXISTS (
        SELECT 1 FROM core.subject_offerings existing
         WHERE existing.tenant_id = mec_tenant
           AND existing.subject_id = subject.id
           AND existing.academic_year_id = year_id
           AND existing.section_id = section.section_id
           AND existing.term_id IS NOT DISTINCT FROM term_value
     );

    UPDATE core.subject_offerings offering
       SET active = true,
           metadata = offering.metadata || jsonb_build_object(
               'source','MEC official odd semester timetable 2026-27',
               'departmentCode',course.department_code),
           updated_at = now()
      FROM matrix_courses course
      JOIN matrix_sections section USING (department_code)
      JOIN core.subjects subject
        ON subject.tenant_id = mec_tenant AND subject.code = course.subject_code
     WHERE offering.tenant_id = mec_tenant
       AND offering.subject_id = subject.id
       AND offering.academic_year_id = year_id
       AND offering.section_id = section.section_id
       AND offering.term_id IS NOT DISTINCT FROM term_value;

    UPDATE core.subject_offerings offering
       SET active = false, updated_at = now(),
           metadata = offering.metadata || '{"replacedByOfficialMatrix":true}'::jsonb
      FROM matrix_sections section, core.subjects subject
     WHERE offering.tenant_id = mec_tenant
       AND offering.section_id = section.section_id
       AND offering.subject_id = subject.id
       AND subject.tenant_id = mec_tenant
       AND NOT EXISTS (
           SELECT 1 FROM matrix_courses wanted
            WHERE wanted.department_code = section.department_code
              AND wanted.subject_code = subject.code
       );

    UPDATE core.teaching_assignments assignment
       SET active = false, updated_at = now(),
           metadata = assignment.metadata || '{"replacedByOfficialMatrix":true}'::jsonb
      FROM core.subject_offerings offering
      JOIN core.subjects subject ON subject.id = offering.subject_id
      JOIN matrix_sections section ON section.section_id = offering.section_id
      JOIN matrix_courses course
        ON course.department_code = section.department_code
       AND course.subject_code = subject.code
      JOIN identity.users official_faculty
        ON lower(official_faculty.email) = lower(course.faculty_email)
     WHERE assignment.tenant_id = mec_tenant
       AND assignment.subject_offering_id = offering.id
       AND assignment.assignment_type = 'primary'
       AND assignment.faculty_user_id <> official_faculty.id;

    INSERT INTO core.teaching_assignments
        (tenant_id, subject_offering_id, faculty_user_id, assignment_type,
         active, assigned_by, metadata)
    SELECT mec_tenant, offering.id, faculty.id, 'primary', true, principal_id,
           jsonb_build_object('source','MEC official odd semester timetable 2026-27')
      FROM matrix_courses course
      JOIN matrix_sections section USING (department_code)
      JOIN core.subjects subject
        ON subject.tenant_id = mec_tenant AND subject.code = course.subject_code
      JOIN core.subject_offerings offering
        ON offering.tenant_id = mec_tenant
       AND offering.subject_id = subject.id
       AND offering.section_id = section.section_id
       AND offering.academic_year_id = year_id
       AND offering.term_id IS NOT DISTINCT FROM term_value
      JOIN identity.users faculty ON lower(faculty.email) = lower(course.faculty_email)
    ON CONFLICT (tenant_id, subject_offering_id, faculty_user_id, assignment_type)
    DO UPDATE SET active = true,
                  metadata = core.teaching_assignments.metadata || EXCLUDED.metadata,
                  updated_at = now();

    DELETE FROM core.subject_offering_workload_requirements requirement
     USING core.subject_offerings offering, matrix_sections section
     WHERE requirement.tenant_id = mec_tenant
       AND offering.id = requirement.subject_offering_id
       AND section.section_id = offering.section_id;

    INSERT INTO core.subject_offering_workload_requirements
        (tenant_id, subject_offering_id, delivery_type, periods_per_week,
         block_size, max_blocks_per_day, required_room_types, metadata, created_by)
    SELECT mec_tenant, offering.id, slot.delivery_type, count(*)::smallint, 1,
           greatest(1, max(day_count.count_for_day))::smallint,
           CASE WHEN slot.delivery_type = 'laboratory'
                THEN ARRAY['laboratory']::text[] ELSE ARRAY[]::text[] END,
           jsonb_build_object('source','MEC official odd semester timetable 2026-27'),
           principal_id
      FROM matrix_slots slot
      JOIN (
          SELECT department_code, subject_code, delivery_type, day_no, count(*) count_for_day
            FROM matrix_slots GROUP BY 1,2,3,4
      ) day_count USING (department_code, subject_code, delivery_type)
      JOIN matrix_sections section USING (department_code)
      JOIN core.subjects subject
        ON subject.tenant_id = mec_tenant AND subject.code = slot.subject_code
      JOIN core.subject_offerings offering
        ON offering.tenant_id = mec_tenant
       AND offering.subject_id = subject.id
       AND offering.section_id = section.section_id
       AND offering.academic_year_id = year_id
       AND offering.term_id IS NOT DISTINCT FROM term_value
     GROUP BY offering.id, slot.delivery_type;

    IF EXISTS (SELECT 1 FROM core.timetable_versions WHERE id = official_version) THEN
        RAISE EXCEPTION 'official timetable version already exists; no records changed';
    END IF;

    INSERT INTO core.timetable_versions
        (id, tenant_id, configuration_id, version_number, label, status,
         rules_snapshot, created_by)
    SELECT official_version, mec_tenant, config_id,
           coalesce(max(version_number),0) + 1,
           'Official MEC matrix · 06 Jul 2026', 'draft',
           jsonb_build_object('source','MEC official odd semester timetable 2026-27',
                              'departments',6,'periods',210), principal_id
      FROM core.timetable_versions WHERE configuration_id = config_id;

    INSERT INTO core.timetable_entries
        (tenant_id, version_id, slot_id, subject_offering_id,
         teaching_assignment_id, room_id, delivery_type, metadata, created_by,
         session_block_id, block_sequence, block_length)
    SELECT mec_tenant, official_version, timetable_slot.id, offering.id,
           assignment.id, section.room_id, matrix_slot.delivery_type,
           jsonb_build_object('source','MEC official odd semester timetable 2026-27',
                              'departmentCode',matrix_slot.department_code,
                              'officialActivity',matrix_slot.activity),
           principal_id, gen_random_uuid(), 1, 1
      FROM matrix_slots matrix_slot
      JOIN matrix_sections section USING (department_code)
      JOIN core.subjects subject
        ON subject.tenant_id = mec_tenant AND subject.code = matrix_slot.subject_code
      JOIN core.subject_offerings offering
        ON offering.tenant_id = mec_tenant
       AND offering.subject_id = subject.id
       AND offering.section_id = section.section_id
       AND offering.academic_year_id = year_id
       AND offering.term_id IS NOT DISTINCT FROM term_value
      JOIN core.teaching_assignments assignment
        ON assignment.tenant_id = mec_tenant
       AND assignment.subject_offering_id = offering.id
       AND assignment.assignment_type = 'primary' AND assignment.active
      JOIN core.timetable_slots timetable_slot
        ON timetable_slot.tenant_id = mec_tenant
       AND timetable_slot.configuration_id = config_id
       AND timetable_slot.day_of_week = matrix_slot.day_no
       AND timetable_slot.sequence = matrix_slot.period_no;

    -- CSE and Cyber share the identical faculty/subject matrix at identical
    -- hours. Mark those rows as one combined class so the faculty app collapses
    -- the two section records into a single teaching card.
    UPDATE core.timetable_entries entry
       SET metadata = entry.metadata || jsonb_build_object(
           'combinedClassCode','CSE-CYBER',
           'combinedClassName','CSE + CYBER - Section A'),
           updated_at = now()
     WHERE entry.version_id = official_version
       AND entry.metadata ->> 'departmentCode' IN ('CSE','CYBER')
       AND EXISTS (
           SELECT 1
             FROM core.timetable_entries counterpart
             JOIN core.teaching_assignments counterpart_assignment
               ON counterpart_assignment.id = counterpart.teaching_assignment_id
             JOIN core.teaching_assignments own_assignment
               ON own_assignment.id = entry.teaching_assignment_id
             JOIN core.subject_offerings counterpart_offering
               ON counterpart_offering.id = counterpart.subject_offering_id
             JOIN core.subjects counterpart_subject
               ON counterpart_subject.id = counterpart_offering.subject_id
             JOIN core.subject_offerings own_offering
               ON own_offering.id = entry.subject_offering_id
             JOIN core.subjects own_subject ON own_subject.id = own_offering.subject_id
            WHERE counterpart.version_id = official_version
              AND counterpart.slot_id = entry.slot_id
              AND counterpart_assignment.faculty_user_id = own_assignment.faculty_user_id
              AND counterpart_subject.code = own_subject.code
              AND counterpart.metadata ->> 'departmentCode' IN ('CSE','CYBER')
              AND counterpart.metadata ->> 'departmentCode'
                    <> entry.metadata ->> 'departmentCode'
       );

    IF (SELECT count(*) FROM core.timetable_entries WHERE version_id = official_version) <> 210 THEN
        RAISE EXCEPTION 'official matrix did not create all 210 timetable entries';
    END IF;

    UPDATE core.timetable_versions
       SET status = 'superseded', updated_at = now()
     WHERE tenant_id = mec_tenant AND status = 'published' AND id <> official_version;
    UPDATE core.timetable_versions
       SET status = 'published', published_by = principal_id,
           published_at = now(), updated_at = now()
     WHERE id = official_version;
END
`$matrix_apply`$;
COMMIT;
"@

[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sql))
