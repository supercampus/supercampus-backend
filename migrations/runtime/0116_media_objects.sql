-- Files kept by the media service when Cloudinary is not configured, refuses
-- an upload, or MEDIA_STORAGE=database. Served publicly by id from
-- GET /api/media/files/{tenant}/{id}/{name}; the random id is the capability,
-- as a Cloudinary URL is. Uploads are capped at 10 MB by the API.
CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.media_objects (
    id uuid PRIMARY KEY,
    tenant_slug text NOT NULL,
    file_name text NOT NULL,
    content_type text NOT NULL,
    byte_size integer NOT NULL CHECK (byte_size > 0 AND byte_size <= 10485760),
    content bytea NOT NULL,
    uploaded_by text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS media_objects_tenant_created_idx
    ON campus_ops.media_objects (tenant_slug, created_at DESC);
