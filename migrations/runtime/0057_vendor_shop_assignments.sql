CREATE TABLE IF NOT EXISTS campus_ops.shop_user_assignments (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    shop_id uuid NOT NULL REFERENCES campus_ops.shops(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    assignment_role text NOT NULL DEFAULT 'owner',
    is_active boolean NOT NULL DEFAULT true,
    assigned_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, shop_id, user_id),
    CONSTRAINT shop_user_assignment_role CHECK (assignment_role IN ('owner', 'captain'))
);

CREATE INDEX IF NOT EXISTS shop_user_assignments_user_idx
    ON campus_ops.shop_user_assignments (tenant_id, user_id, is_active);
