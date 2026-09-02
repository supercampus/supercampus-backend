CREATE TABLE IF NOT EXISTS core.marks_batches (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
  department_id uuid NOT NULL,
  subject_code text NOT NULL,
  subject_name text NOT NULL,
  assessment_type text NOT NULL,
  maximum_marks double precision NOT NULL CHECK (maximum_marks > 0),
  entries jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(entries) = 'array'),
  submitted_by text NOT NULL,
  status text NOT NULL CHECK (status IN ('submitted_to_advisor','submitted_to_hod','submitted_to_principal','approved','returned')),
  review_note text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  FOREIGN KEY (tenant_id, department_id) REFERENCES core.departments(tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.marks_batch_reviews (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
  batch_id uuid NOT NULL REFERENCES core.marks_batches(id) ON DELETE CASCADE,
  actor_user_id text NOT NULL,
  actor_role text NOT NULL,
  decision text NOT NULL CHECK (decision IN ('submit','approve','reject')),
  from_status text NOT NULL,
  to_status text NOT NULL,
  note text,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS marks_batches_queue_idx
  ON core.marks_batches(tenant_id, department_id, status, created_at DESC);
