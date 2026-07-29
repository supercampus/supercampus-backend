CREATE SCHEMA IF NOT EXISTS configuration;
CREATE TABLE IF NOT EXISTS configuration.documents (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    namespace text NOT NULL,
    key text NOT NULL,
    version integer NOT NULL CHECK (version > 0),
    status text NOT NULL DEFAULT 'draft',
    value jsonb NOT NULL,
    created_by uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, namespace, key, version)
);