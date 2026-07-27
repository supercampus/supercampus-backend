ALTER TABLE students ALTER COLUMN email SET NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS students_email_unique_idx
  ON students (lower(email));
