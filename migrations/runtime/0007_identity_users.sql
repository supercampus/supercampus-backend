-- Database-backed identities and tenant memberships.
-- Passwords are produced and verified by PostgreSQL pgcrypto's adaptive crypt().
ALTER TABLE platform.tenants
    ADD COLUMN IF NOT EXISTS code text,
    ADD COLUMN IF NOT EXISTS city text NOT NULL DEFAULT '';

UPDATE platform.tenants
SET code = upper(replace(slug, '-', '_'))
WHERE code IS NULL OR btrim(code) = '';

ALTER TABLE platform.tenants
    ALTER COLUMN code SET NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS tenants_code_unique_idx
    ON platform.tenants (lower(code));

CREATE TABLE IF NOT EXISTS identity.users (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email text NOT NULL UNIQUE CHECK (email = lower(email)),
    password_hash text NOT NULL,
    display_name text NOT NULL,
    initials text NOT NULL,
    account_type text NOT NULL DEFAULT 'staff',
    active boolean NOT NULL DEFAULT true,
    profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

CREATE TABLE IF NOT EXISTS identity.tenant_memberships (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE CASCADE,
    roles text[] NOT NULL CHECK (cardinality(roles) > 0),
    active boolean NOT NULL DEFAULT true,
    is_primary boolean NOT NULL DEFAULT false,
    profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE INDEX IF NOT EXISTS tenant_memberships_user_idx
    ON identity.tenant_memberships (user_id, active, is_primary DESC);

CREATE UNIQUE INDEX IF NOT EXISTS tenant_memberships_one_primary_idx
    ON identity.tenant_memberships (user_id)
    WHERE is_primary AND active;

-- Existing sessions remain valid; new sessions use the UUID identity as text.
CREATE INDEX IF NOT EXISTS users_active_email_idx
    ON identity.users (email)
    WHERE active;
