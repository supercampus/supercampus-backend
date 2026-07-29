CREATE SCHEMA IF NOT EXISTS workflow;
CREATE TABLE IF NOT EXISTS workflow.instances (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    definition_key text NOT NULL,
    definition_version integer NOT NULL,
    entity_type text NOT NULL,
    entity_id uuid NOT NULL,
    state text NOT NULL,
    context jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);