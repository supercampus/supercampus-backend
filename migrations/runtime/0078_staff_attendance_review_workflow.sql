-- Governed subject attendance workflow: faculty -> advisor -> HOD -> principal.
ALTER TABLE campus_ops.attendance_sessions
  DROP CONSTRAINT IF EXISTS attendance_sessions_status_check;

UPDATE campus_ops.attendance_sessions
   SET status = 'submitted_to_hod'
 WHERE status = 'published_to_hod';

ALTER TABLE campus_ops.attendance_sessions
  ADD CONSTRAINT attendance_sessions_status_check CHECK (
    status IN (
      'draft', 'submitted_to_advisor', 'submitted_to_hod',
      'submitted_to_principal', 'approved', 'returned'
    )
  );

CREATE TABLE IF NOT EXISTS campus_ops.attendance_session_reviews (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
  session_id uuid NOT NULL REFERENCES campus_ops.attendance_sessions(id) ON DELETE CASCADE,
  actor_user_id text NOT NULL,
  actor_role text NOT NULL,
  decision text NOT NULL CHECK (decision IN ('submit', 'approve', 'enquire', 'reject')),
  from_status text NOT NULL,
  to_status text NOT NULL,
  note text,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS attendance_session_reviews_session_idx
  ON campus_ops.attendance_session_reviews (tenant_id, session_id, created_at);

UPDATE authz.permission_templates
   SET display_name = 'Submit attendance',
       description = 'Submit or review attendance through advisor, HOD and principal approval'
 WHERE permission_key = 'attendance.session.publish';
