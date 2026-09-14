-- Marks recorded by the class advisor for students in their assigned class.
-- Assessment titles remain data so an advisor can create institution-specific
-- tests without a schema change.
CREATE TABLE IF NOT EXISTS core.student_assessment_marks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_id uuid NOT NULL,
    advisor_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    assessment_kind text NOT NULL
        CHECK (assessment_kind IN ('semester', 'internal', 'test')),
    title text NOT NULL,
    semester smallint CHECK (semester IS NULL OR semester BETWEEN 1 AND 12),
    marks_obtained double precision NOT NULL CHECK (marks_obtained >= 0),
    maximum_marks double precision NOT NULL CHECK (maximum_marks > 0),
    notes text,
    assessed_on date,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students (tenant_id, id) ON DELETE CASCADE,
    CHECK (marks_obtained <= maximum_marks)
);

CREATE INDEX IF NOT EXISTS student_assessment_marks_student_idx
    ON core.student_assessment_marks
       (tenant_id, student_id, semester, assessed_on DESC, created_at DESC);
