CREATE SCHEMA IF NOT EXISTS audit;
CREATE TABLE IF NOT EXISTS audit.entries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    actor_id uuid,
    action text NOT NULL,
    resource_type text NOT NULL,
    resource_id text,
    before_value jsonb,
    after_value jsonb,
    correlation_id uuid,
    occurred_at timestamptz NOT NULL DEFAULT now()
);