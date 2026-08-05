-- Control-plane registry for database-per-institution routing.
-- Credentials remain in CONTROL_DATABASE_URL; only safe logical database names are stored here.
CREATE TABLE IF NOT EXISTS platform.tenant_databases (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    database_name text NOT NULL UNIQUE,
    status text NOT NULL DEFAULT 'provisioning',
    migration_version bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (database_name ~ '^[A-Za-z0-9_]{1,63}$'),
    CHECK (status IN ('provisioning', 'active', 'suspended', 'failed'))
);

CREATE INDEX IF NOT EXISTS tenant_databases_status_idx
    ON platform.tenant_databases (status, tenant_id);