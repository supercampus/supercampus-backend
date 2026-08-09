-- Self-service password reset.
-- Raw reset tokens are never stored. Only the SHA-256 digest is persisted, mirroring
-- the refresh-token handling in identity.auth_sessions.
CREATE TABLE IF NOT EXISTS identity.password_reset_tokens (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    CHECK (octet_length(token_hash) = 32),
    CHECK (expires_at > created_at)
);

-- Supports the per-account request throttle, which counts recent rows for one user.
CREATE INDEX IF NOT EXISTS password_reset_tokens_user_recent_idx
    ON identity.password_reset_tokens (user_id, created_at DESC);

-- Supports expiry sweeps.
CREATE INDEX IF NOT EXISTS password_reset_tokens_expiry_idx
    ON identity.password_reset_tokens (expires_at)
    WHERE consumed_at IS NULL;
