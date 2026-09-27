-- Bring an older campus_ops.library_announcements up to the 0083/0084 shape.
-- 0083 used CREATE TABLE IF NOT EXISTS, so an installation whose table came
-- from an earlier definition kept created_by/decided_by as uuid and lacks the
-- later columns. Reads then fail on text = uuid and posts on inserting text.
-- Idempotent: every step checks the current shape first.
DO $$
DECLARE
    target regclass := to_regclass('campus_ops.library_announcements');
    fk record;
BEGIN
    IF target IS NULL THEN
        RETURN;
    END IF;

    ALTER TABLE campus_ops.library_announcements
        ADD COLUMN IF NOT EXISTS book_title text,
        ADD COLUMN IF NOT EXISTS author text,
        ADD COLUMN IF NOT EXISTS created_by_name text,
        ADD COLUMN IF NOT EXISTS decision_note text,
        ADD COLUMN IF NOT EXISTS decided_by text,
        ADD COLUMN IF NOT EXISTS decided_at timestamptz,
        ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();

    -- Foreign keys on the id columns would block the type change; the app
    -- stores these ids as text like every other campus_ops table.
    FOR fk IN
        SELECT con.conname
        FROM pg_constraint con
        JOIN pg_attribute att
          ON att.attrelid = con.conrelid AND att.attnum = ANY (con.conkey)
        WHERE con.conrelid = target
          AND con.contype = 'f'
          AND att.attname IN ('created_by', 'decided_by')
    LOOP
        EXECUTE format(
            'ALTER TABLE campus_ops.library_announcements DROP CONSTRAINT %I',
            fk.conname
        );
    END LOOP;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'campus_ops' AND table_name = 'library_announcements'
          AND column_name = 'created_by' AND data_type <> 'text'
    ) THEN
        ALTER TABLE campus_ops.library_announcements
            ALTER COLUMN created_by TYPE text USING created_by::text;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'campus_ops' AND table_name = 'library_announcements'
          AND column_name = 'decided_by' AND data_type <> 'text'
    ) THEN
        ALTER TABLE campus_ops.library_announcements
            ALTER COLUMN decided_by TYPE text USING decided_by::text;
    END IF;
END $$;
