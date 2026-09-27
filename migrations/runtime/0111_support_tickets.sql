-- Help requests raised from the app and routed to the role that handles them.
CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.support_tickets (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    requester_user_id text NOT NULL,
    requester_name text,
    requester_email text,
    requester_role text,
    category text NOT NULL,
    subject text NOT NULL,
    message text NOT NULL,
    context jsonb NOT NULL DEFAULT '{}'::jsonb,
    assigned_role text NOT NULL,
    status text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'resolved', 'closed')),
    resolution_note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS support_tickets_requester_idx
    ON campus_ops.support_tickets (tenant_id, requester_user_id);

CREATE INDEX IF NOT EXISTS support_tickets_inbox_idx
    ON campus_ops.support_tickets (tenant_id, assigned_role, status);
