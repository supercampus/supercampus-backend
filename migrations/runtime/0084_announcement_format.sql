-- Common announcement format used by every publisher surface.
ALTER TABLE campus_ops.library_announcements
    ADD COLUMN IF NOT EXISTS announcement_type text NOT NULL DEFAULT 'Announcement',
    ADD COLUMN IF NOT EXISTS announcement_date date NOT NULL DEFAULT CURRENT_DATE,
    ADD COLUMN IF NOT EXISTS attachment_name text,
    ADD COLUMN IF NOT EXISTS attachment_url text;

ALTER TABLE campus_ops.library_announcements
    DROP CONSTRAINT IF EXISTS library_announcements_type_not_blank;
ALTER TABLE campus_ops.library_announcements
    ADD CONSTRAINT library_announcements_type_not_blank
    CHECK (length(btrim(announcement_type)) > 0);
