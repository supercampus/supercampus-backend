CREATE TABLE IF NOT EXISTS tenants (
  id uuid PRIMARY KEY,
  code varchar(20) NOT NULL UNIQUE,
  name text NOT NULL,
  city text NOT NULL,
  is_active boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO tenants (id, code, name, city) VALUES
  ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'SVCE', 'Sri Venkateswara College of Engineering', 'Chennai'),
  ('bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb', 'REC', 'Rajalakshmi Engineering College', 'Chennai')
ON CONFLICT (id) DO UPDATE SET
  code = EXCLUDED.code,
  name = EXCLUDED.name,
  city = EXCLUDED.city,
  updated_at = now();

ALTER TABLE students ADD COLUMN IF NOT EXISTS tenant_id uuid REFERENCES tenants(id);
ALTER TABLE students ADD COLUMN IF NOT EXISTS email text;
ALTER TABLE students ADD COLUMN IF NOT EXISTS password_hash text;
ALTER TABLE students ADD COLUMN IF NOT EXISTS is_active boolean NOT NULL DEFAULT true;

UPDATE students
SET tenant_id = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    email = 'arun.kumar@svce.edu.in',
    password_hash = '$2b$12$MJVUf5IF0ZvPPw3HkZxWhuat1fdlyiPr6VlHQAeTLnp4mktZXHWqa'
WHERE id = '11111111-1111-4111-8111-111111111111';

ALTER TABLE students ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE students ALTER COLUMN password_hash SET NOT NULL;
ALTER TABLE students DROP CONSTRAINT IF EXISTS students_roll_number_key;

CREATE UNIQUE INDEX IF NOT EXISTS students_tenant_roll_idx ON students (tenant_id, roll_number);
CREATE INDEX IF NOT EXISTS students_tenant_active_idx ON students (tenant_id, is_active) WHERE is_active = true;

INSERT INTO students (
  id, tenant_id, roll_number, email, password_hash, name, initials,
  college_code, college_name, department, study_year, is_active
) VALUES (
  '22222222-2222-4222-8222-222222222222',
  'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
  '23CS102',
  'priya.sharma@rec.edu.in',
  '$2b$12$7SvJuERvvyEmDnIKLER1SO.omR/0g29VnCBQ8m1jhFWdeVVibB75q',
  'Priya Sharma',
  'PS',
  'REC',
  'Rajalakshmi Engineering College',
  'Computer Science & Engineering',
  '3rd Year',
  true
)
ON CONFLICT (id) DO UPDATE SET
  tenant_id = EXCLUDED.tenant_id,
  roll_number = EXCLUDED.roll_number,
  email = EXCLUDED.email,
  password_hash = EXCLUDED.password_hash,
  name = EXCLUDED.name,
  initials = EXCLUDED.initials,
  college_code = EXCLUDED.college_code,
  college_name = EXCLUDED.college_name,
  department = EXCLUDED.department,
  study_year = EXCLUDED.study_year,
  is_active = true,
  updated_at = now();

INSERT INTO student_app_state (student_id, state)
VALUES (
  '22222222-2222-4222-8222-222222222222',
  '{
    "persona":"dayscholar",
    "gp":{"status":"none","type":null,"early":false,"step":0},
    "paid":{"tuition":true,"hostel":true,"transport":false,"exam":true},
    "pay":{"comp":null,"step":0,"plan":null,"mode":null},
    "refunds":{},"condonation":"none","examReg":0,"reval":{},
    "asg":{"a3":"none"},"changeNotice":true,"mess":false,"hostelLeave":0,
    "hostelTickets":[],"tripStep":0,"breakdown":false,"docReq":[],"placeApp":0,"feedback":0
  }'::jsonb
)
ON CONFLICT (student_id) DO NOTHING;
