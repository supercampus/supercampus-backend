-- Visitor passes, and the campus geofence that gate-in already pretended to
-- enforce.
--
-- Two separate things, in one migration because they are the two halves of the
-- same gate: who may come in, and from where a pass may be raised.
--
-- 1. campus_ops.visitor_passes
--
-- A visitor is not a member of the institution, so they have no account, no
-- role and no membership — everything about them lives on the pass itself. Two
-- kinds, which differ in who raises them and nothing else structurally:
--
--   parent : raised by a student for their own guardian
--   guest  : raised by an administrator for anyone else
--
-- Both wait on an administrator's approval, and on approval a QR is issued and
-- the rendered pass is sent to the visitor over WhatsApp. The card is silver
-- for a parent and gold for a guest; that is a presentation detail derived from
-- `visitor_kind`, so it is not stored.
--
-- 2. The geofence
--
-- campus_ops.daily_access_passes has recorded activated_latitude and
-- activated_longitude since it was created, and nothing has ever read them. A
-- student could activate the next day's gate-in QR from home by posting any two
-- numbers. The fence lives on core.campuses.metadata so a tenant can move or
-- resize it without a migration, and so a multi-campus institution can hold one
-- per campus.

CREATE TABLE IF NOT EXISTS campus_ops.visitor_passes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,

    visitor_kind text NOT NULL CHECK (visitor_kind IN ('parent', 'guest')),
    visitor_name text NOT NULL,
    -- E.164, because it is dialled by Twilio rather than read by a person.
    visitor_phone text NOT NULL,
    purpose text NOT NULL,

    -- Who is being visited. For a parent pass this is the student; for a guest
    -- pass it is the member of staff receiving them.
    host_user_id text NOT NULL,
    host_name text NOT NULL,

    -- Who raised it, which is not always the host: a parent pass is raised by
    -- the student, a guest pass by an administrator.
    requested_by text NOT NULL,

    visit_from timestamptz NOT NULL,
    visit_until timestamptz NOT NULL,

    state text NOT NULL DEFAULT 'pending_admin'
        CHECK (state IN ('pending_admin', 'approved', 'rejected')),

    -- Only ever the hash. The QR itself is shown once, to the visitor.
    qr_token_hash text,
    -- The rendered card, once it has been generated and stored.
    pass_image_url text,

    -- Delivery is tracked separately from approval: a pass can be validly
    -- approved and still have failed to reach the visitor's phone, and an
    -- administrator needs to be able to see that difference.
    delivery_state text NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'sent', 'failed', 'not_configured')),
    delivery_error text,
    delivered_at timestamptz,

    decided_by text,
    decision_note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT visitor_passes_window_check CHECK (visit_until > visit_from)
);

CREATE INDEX IF NOT EXISTS visitor_passes_tenant_state_idx
    ON campus_ops.visitor_passes (tenant_id, state, visit_from DESC);

CREATE INDEX IF NOT EXISTS visitor_passes_host_idx
    ON campus_ops.visitor_passes (tenant_id, host_user_id);

-- The scan endpoint looks a token up across every kind of pass, so this index
-- carries the same shape as the one on gatepass_requests.
CREATE INDEX IF NOT EXISTS visitor_passes_token_idx
    ON campus_ops.visitor_passes (tenant_id, qr_token_hash)
    WHERE qr_token_hash IS NOT NULL;

ALTER TABLE campus_ops.visitor_passes ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON campus_ops.visitor_passes;
CREATE POLICY tenant_isolation ON campus_ops.visitor_passes
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

-- A visitor has no user account, so gate_movements.user_id cannot hold them and
-- its request_id points at gatepass_requests. Visitor movements are recorded
-- against the pass instead.
ALTER TABLE campus_ops.gate_movements
    ADD COLUMN IF NOT EXISTS visitor_pass_id uuid
        REFERENCES campus_ops.visitor_passes(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS gate_movements_visitor_idx
    ON campus_ops.gate_movements (tenant_id, visitor_pass_id)
    WHERE visitor_pass_id IS NOT NULL;

-- gate_movements.user_id is NOT NULL and a visitor has no account, so a visitor
-- movement stores the pass id in that column as well. Recording the pass on
-- both sides keeps the existing "movements for this person" queries working
-- without teaching every one of them about visitors.
COMMENT ON COLUMN campus_ops.gate_movements.visitor_pass_id IS
    'Set when the movement belongs to a visitor rather than a member of the institution.';
