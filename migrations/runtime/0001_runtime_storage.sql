CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA IF NOT EXISTS platform;
CREATE SCHEMA IF NOT EXISTS configuration;
CREATE SCHEMA IF NOT EXISTS identity;

CREATE TABLE IF NOT EXISTS platform.tenants (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug text NOT NULL UNIQUE,
    name text NOT NULL,
    status text NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS platform.dynamic_records (
    id uuid PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    module_key text NOT NULL,
    record_type text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS dynamic_records_tenant_module_idx
    ON platform.dynamic_records (tenant_id, module_key, updated_at DESC);

CREATE TABLE IF NOT EXISTS configuration.runtime_documents (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    namespace text NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, namespace)
);

CREATE TABLE IF NOT EXISTS identity.local_sessions (
    token_hash bytea PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    student jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS local_sessions_expiry_idx
    ON identity.local_sessions (expires_at);

CREATE TABLE IF NOT EXISTS identity.ui_states (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    state jsonb NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

INSERT INTO platform.tenants (slug, name)
VALUES ('tenant-local', 'SuperCampus Local')
ON CONFLICT (slug) DO NOTHING;