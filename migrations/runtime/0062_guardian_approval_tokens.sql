-- Parent approval of an outpass, without an account.
--
-- The outpass chain is ["parent", "warden", "security"], and step one belongs
-- to someone who has no login and never will: a guardian reached over WhatsApp.
-- Until now nothing could satisfy that step, so every outpass a hosteller
-- raised stopped at `pending_parent` and stayed there.
--
-- A token stands in for the account. It is minted when the request is raised,
-- sent to the guardian's phone as a link, and spent the first time it is used.
-- Deliberately narrow: one token authorises one decision on one request, and
-- nothing else in the system.
--
-- Only the hash is stored, exactly as gatepass_requests.qr_token_hash and
-- identity.password_reset_tokens already do — a leaked database row must not
-- become a working approval link.

CREATE TABLE IF NOT EXISTS campus_ops.guardian_approval_tokens (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    request_id uuid NOT NULL
        REFERENCES campus_ops.gatepass_requests(id) ON DELETE CASCADE,

    -- Which step of the chain this token may answer. Only 'parent' today, but
    -- the column exists so a tenant that adds an off-platform approver later
    -- does not need a second table.
    step_key text NOT NULL DEFAULT 'parent',

    -- Recorded as sent, so an administrator can see where the link went even
    -- after the guardian's number is later corrected on the student record.
    guardian_name text NOT NULL,
    guardian_phone text NOT NULL,

    token_hash text NOT NULL,
    expires_at timestamptz NOT NULL,

    -- Spent, not deleted: the audit trail of who approved what has to survive
    -- the token itself.
    used_at timestamptz,
    decision text CHECK (decision IN ('approved', 'rejected')),

    delivery_state text NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'sent', 'failed', 'not_configured')),
    delivery_error text,

    created_at timestamptz NOT NULL DEFAULT now()
);

-- The lookup is by hash on every visit to the link, and it must be unique or
-- two requests could share an approval.
CREATE UNIQUE INDEX IF NOT EXISTS guardian_approval_tokens_hash_idx
    ON campus_ops.guardian_approval_tokens (token_hash);

CREATE INDEX IF NOT EXISTS guardian_approval_tokens_request_idx
    ON campus_ops.guardian_approval_tokens (tenant_id, request_id);

ALTER TABLE campus_ops.guardian_approval_tokens ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON campus_ops.guardian_approval_tokens;
CREATE POLICY tenant_isolation ON campus_ops.guardian_approval_tokens
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

-- core.guardians exists but has never been populated, and the outpass flow is
-- the first thing to need it. A student may have more than one guardian; the
-- primary is the one the approval link is sent to.
ALTER TABLE core.guardians
    ADD COLUMN IF NOT EXISTS student_id uuid,
    ADD COLUMN IF NOT EXISTS relationship text,
    ADD COLUMN IF NOT EXISTS is_primary boolean NOT NULL DEFAULT false;

CREATE INDEX IF NOT EXISTS guardians_student_idx
    ON core.guardians (tenant_id, student_id)
    WHERE student_id IS NOT NULL;

-- One primary guardian per student, so "who do we send the link to" has exactly
-- one answer rather than depending on row order.
CREATE UNIQUE INDEX IF NOT EXISTS guardians_primary_idx
    ON core.guardians (tenant_id, student_id)
    WHERE is_primary AND student_id IS NOT NULL;
