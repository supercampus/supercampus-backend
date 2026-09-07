ALTER TABLE identity.auth_sessions
    ADD COLUMN IF NOT EXISTS device_id text,
    ADD COLUMN IF NOT EXISTS device_name text;

CREATE INDEX IF NOT EXISTS auth_sessions_active_device_idx
    ON identity.auth_sessions (tenant_id, user_id, device_id)
    WHERE revoked_at IS NULL;
