CREATE SCHEMA IF NOT EXISTS identity;
CREATE TABLE IF NOT EXISTS identity.users (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    external_subject text NOT NULL,
    email text,
    display_name text NOT NULL,
    status text NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, external_subject)
);