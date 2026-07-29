CREATE SCHEMA IF NOT EXISTS module_registry;
CREATE TABLE IF NOT EXISTS module_registry.installations (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    module_key text NOT NULL,
    installed_version text NOT NULL,
    status text NOT NULL DEFAULT 'inactive',
    configuration jsonb NOT NULL DEFAULT '{}'::jsonb,
    installed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, module_key)
);