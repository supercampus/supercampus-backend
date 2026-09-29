-- Ad-hoc payment requests and the Razorpay order ledger.
--
-- A payment request asks one or more students to pay a fixed amount for a
-- purpose (a fine, an electricity bill, ...). Each targeted student gets a
-- payer row whose status moves pending -> paid (online through Razorpay or
-- marked paid by the accounts office) or cancelled with the request.
--
-- campus_ops.razorpay_orders records every Razorpay order the API creates so
-- the Online Payments page can track capture, fulfilment (wallet credited /
-- fee recorded / request paid), recovery of captured-but-unfulfilled orders
-- and settlement. Settlement columns stay NULL until a reconciliation run
-- reads them from Razorpay; nothing is inferred.
--
-- The API applies this file at runtime (payment_requests::ensure_schema) for
-- databases the migrator cannot reach. Safe to run more than once.
CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.payment_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    purpose text NOT NULL DEFAULT 'other',
    title text NOT NULL,
    description text NOT NULL,
    amount numeric(12,2) NOT NULL CHECK (amount > 0),
    currency text NOT NULL DEFAULT 'INR',
    due_date date,
    show_on_dashboard boolean NOT NULL DEFAULT true,
    target_mode text NOT NULL DEFAULT 'all'
        CHECK (target_mode IN ('all', 'students', 'cohort')),
    target_departments text[] NOT NULL DEFAULT '{}',
    target_years text[] NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'closed', 'cancelled')),
    created_by text NOT NULL,
    created_by_name text,
    cancelled_by text,
    cancel_reason text,
    cancelled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS payment_requests_tenant_created_idx
    ON campus_ops.payment_requests (tenant_id, created_at DESC);

CREATE TABLE IF NOT EXISTS campus_ops.payment_request_payers (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL,
    request_id uuid NOT NULL REFERENCES campus_ops.payment_requests(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    student_id text,
    student_number text,
    student_name text NOT NULL DEFAULT '',
    department text,
    year_of_study text,
    amount numeric(12,2) NOT NULL CHECK (amount > 0),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'paid', 'cancelled')),
    paid_at timestamptz,
    payment_method text,
    payment_reference text,
    razorpay_order_id text,
    razorpay_payment_id text,
    marked_by text,
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, request_id, user_id)
);

CREATE INDEX IF NOT EXISTS payment_request_payers_user_idx
    ON campus_ops.payment_request_payers (tenant_id, lower(user_id), status);

CREATE TABLE IF NOT EXISTS campus_ops.razorpay_orders (
    tenant_id uuid NOT NULL,
    order_id text NOT NULL,
    user_id text NOT NULL,
    user_name text,
    user_email text,
    user_number text,
    purpose text NOT NULL,
    reference_id text,
    shop_key text,
    receipt text,
    amount_paise bigint NOT NULL,
    currency text NOT NULL DEFAULT 'INR',
    -- created: order exists, no payment known; captured: money captured;
    -- failed: every known attempt failed; refunded: Razorpay refunded it.
    status text NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'authorized', 'captured', 'failed', 'refunded')),
    payment_id text,
    payment_method text,
    error_code text,
    error_description text,
    captured_at timestamptz,
    -- Whether the campus side effect (wallet credit, fee record, request
    -- paid) has been applied, and whether a reconciliation had to do it.
    fulfilled boolean NOT NULL DEFAULT false,
    fulfilled_at timestamptz,
    recovered boolean NOT NULL DEFAULT false,
    fee_paise bigint,
    tax_paise bigint,
    settlement_id text,
    settled boolean,
    settled_at timestamptz,
    last_synced_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, order_id)
);

CREATE INDEX IF NOT EXISTS razorpay_orders_tenant_created_idx
    ON campus_ops.razorpay_orders (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS razorpay_orders_payment_idx
    ON campus_ops.razorpay_orders (tenant_id, payment_id);
