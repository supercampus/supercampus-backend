-- Student-scoped WhatsApp workflows for guardians.
--
-- Delivery rows snapshot the selected primary guardian and provide one durable
-- idempotency key per business event.  A retry can therefore never drift to a
-- different student or phone number after an administrator edits the master.
CREATE TABLE IF NOT EXISTS campus_ops.guardian_whatsapp_deliveries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_id uuid NOT NULL,
    student_user_id text NOT NULL,
    guardian_id uuid NOT NULL,
    guardian_name text NOT NULL,
    guardian_phone text NOT NULL,
    event_type text NOT NULL,
    event_key text NOT NULL,
    template_name text,
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'processing', 'retrying', 'sent', 'failed')),
    attempt_count integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    locked_at timestamptz,
    provider_message_id text,
    last_error text,
    sent_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, event_key),
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, guardian_id)
        REFERENCES core.guardians(tenant_id, id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS guardian_whatsapp_delivery_queue_idx
    ON campus_ops.guardian_whatsapp_deliveries
       (tenant_id, next_attempt_at, created_at)
    WHERE status IN ('queued', 'retrying');

CREATE TABLE IF NOT EXISTS campus_ops.guardian_fee_payment_links (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    fee_record_id uuid NOT NULL,
    student_id uuid NOT NULL,
    student_user_id text NOT NULL,
    student_number text NOT NULL,
    student_email text,
    guardian_id uuid NOT NULL,
    guardian_name text NOT NULL,
    guardian_phone text NOT NULL,
    amount_paise bigint NOT NULL CHECK (amount_paise > 0),
    currency text NOT NULL DEFAULT 'INR',
    provider_link_id text NOT NULL,
    provider_short_url text NOT NULL,
    payment_id text,
    status text NOT NULL DEFAULT 'issued'
        CHECK (status IN ('issued', 'paid', 'cancelled', 'expired', 'failed')),
    paid_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, fee_record_id, amount_paise),
    UNIQUE (provider_link_id),
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, guardian_id)
        REFERENCES core.guardians(tenant_id, id) ON DELETE RESTRICT
);

ALTER TABLE campus_ops.guardian_whatsapp_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE campus_ops.guardian_fee_payment_links ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON campus_ops.guardian_whatsapp_deliveries;
CREATE POLICY tenant_isolation ON campus_ops.guardian_whatsapp_deliveries
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

DROP POLICY IF EXISTS tenant_isolation ON campus_ops.guardian_fee_payment_links;
CREATE POLICY tenant_isolation ON campus_ops.guardian_fee_payment_links
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
