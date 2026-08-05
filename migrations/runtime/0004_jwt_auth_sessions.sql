CREATE TABLE IF NOT EXISTS identity.auth_sessions (
    id uuid PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    roles text[] NOT NULL CHECK (cardinality(roles) > 0),
    profile jsonb NOT NULL,
    refresh_token_hash bytea NOT NULL UNIQUE,
    previous_refresh_token_hash bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    rotated_at timestamptz,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    CHECK (octet_length(refresh_token_hash) = 32),
    CHECK (
        previous_refresh_token_hash IS NULL
        OR octet_length(previous_refresh_token_hash) = 32
    )
);

CREATE INDEX IF NOT EXISTS auth_sessions_active_user_idx
    ON identity.auth_sessions (tenant_id, user_id, expires_at DESC)
    WHERE revoked_at IS NULL;

CREATE INDEX IF NOT EXISTS auth_sessions_expiry_idx
    ON identity.auth_sessions (expires_at);

CREATE INDEX IF NOT EXISTS auth_sessions_previous_refresh_idx
    ON identity.auth_sessions (previous_refresh_token_hash)
    WHERE previous_refresh_token_hash IS NOT NULL;
