CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.hostel_dining_settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    menu_enabled boolean NOT NULL DEFAULT true,
    mess_enabled boolean NOT NULL DEFAULT true,
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS campus_ops.hostel_fee_entitlements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_user_id text NOT NULL,
    valid_from date NOT NULL,
    valid_until date NOT NULL,
    payment_reference text,
    status text NOT NULL DEFAULT 'paid' CHECK (status IN ('paid','revoked')),
    marked_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until >= valid_from)
);
CREATE INDEX IF NOT EXISTS hostel_fee_entitlements_student_idx
    ON campus_ops.hostel_fee_entitlements(tenant_id, student_user_id, valid_until DESC);

CREATE TABLE IF NOT EXISTS campus_ops.hostel_service_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    requester_user_id text NOT NULL,
    requester_name text NOT NULL,
    request_kind text NOT NULL CHECK (request_kind IN ('complaint','room_change','visitor','clearance')),
    status text NOT NULL DEFAULT 'submitted',
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS hostel_service_requests_requester_idx
    ON campus_ops.hostel_service_requests(tenant_id, requester_user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS campus_ops.hostel_meal_tokens (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_user_id text NOT NULL,
    service_date date NOT NULL,
    meal_type text NOT NULL CHECK (meal_type IN ('breakfast','lunch','dinner')),
    qr_payload text NOT NULL UNIQUE,
    valid_from timestamptz NOT NULL,
    valid_until timestamptz NOT NULL,
    redeemed_at timestamptz,
    redeemed_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant_id, student_user_id, service_date, meal_type)
);
CREATE INDEX IF NOT EXISTS hostel_meal_tokens_qr_idx
    ON campus_ops.hostel_meal_tokens(tenant_id, qr_payload);

INSERT INTO campus_ops.hostel_dining_settings(tenant_id)
SELECT id FROM platform.tenants
ON CONFLICT(tenant_id) DO NOTHING;
