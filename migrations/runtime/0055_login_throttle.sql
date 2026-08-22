-- Distributed, privacy-preserving account throttle for password authentication.
-- The API stores SHA-256(email + tenant selector), never the raw identifier.
CREATE TABLE IF NOT EXISTS identity.login_throttle (
    identifier_hash bytea PRIMARY KEY,
    failure_count integer NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    window_started_at timestamptz NOT NULL DEFAULT now(),
    blocked_until timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (octet_length(identifier_hash) = 32)
);

CREATE INDEX IF NOT EXISTS login_throttle_cleanup_idx
    ON identity.login_throttle (updated_at);
