CREATE TABLE IF NOT EXISTS students (
  id uuid PRIMARY KEY,
  roll_number text NOT NULL UNIQUE,
  name text NOT NULL,
  initials varchar(4) NOT NULL,
  college_code text NOT NULL,
  college_name text NOT NULL,
  department text NOT NULL,
  study_year text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS student_app_state (
  student_id uuid PRIMARY KEY REFERENCES students(id) ON DELETE CASCADE,
  state jsonb NOT NULL,
  version integer NOT NULL DEFAULT 1 CHECK (version > 0),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS activity_events (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  student_id uuid NOT NULL REFERENCES students(id) ON DELETE CASCADE,
  event_type varchar(80) NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS activity_events_student_created_idx
  ON activity_events (student_id, created_at DESC);

INSERT INTO students (
  id, roll_number, name, initials, college_code, college_name, department, study_year
) VALUES (
  '11111111-1111-4111-8111-111111111111',
  '22EC101',
  'Arun Kumar S',
  'AK',
  'SVCE',
  'Sri Venkateswara College of Engineering',
  'Electronics & Communication',
  '4th Year'
)
ON CONFLICT (id) DO UPDATE SET
  roll_number = EXCLUDED.roll_number,
  name = EXCLUDED.name,
  initials = EXCLUDED.initials,
  college_code = EXCLUDED.college_code,
  college_name = EXCLUDED.college_name,
  department = EXCLUDED.department,
  study_year = EXCLUDED.study_year,
  updated_at = now();

INSERT INTO student_app_state (student_id, state)
VALUES (
  '11111111-1111-4111-8111-111111111111',
  '{
    "persona":"hosteller",
    "gp":{"status":"pending","type":"Weekend Leave","early":true,"step":2},
    "paid":{"tuition":true,"hostel":false,"transport":true,"exam":false},
    "pay":{"comp":null,"step":0,"plan":null,"mode":null},
    "refunds":{},
    "condonation":"none",
    "examReg":0,
    "reval":{},
    "asg":{"a3":"none"},
    "changeNotice":true,
    "mess":true,
    "hostelLeave":0,
    "hostelTickets":[{"id":"HST-2291","cat":"Electrical","text":"Tube light not working in Room B-214","status":"In Progress"}],
    "tripStep":1,
    "breakdown":true,
    "docReq":[{"id":"DOC-4410","type":"Bonafide Certificate","on":"18 Jul","status":"Ready"}],
    "placeApp":0,
    "feedback":0
  }'::jsonb
)
ON CONFLICT (student_id) DO NOTHING;
