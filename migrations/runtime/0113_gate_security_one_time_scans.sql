-- Gate security: one-time scans, the movement log, and walk-in visitors.
--
-- * A walk-in visitor is registered by the guard at the gate: no invitation,
--   no QR, gate-in recorded at once. `entry_mode` tells walk-ins apart from
--   invited visitors; vehicle number and an ID note are optional.
-- * The state check is restored to the full 0108 list.
-- * Indexes for the replay checks (per pass / per member per day) and for the
--   tenant-wide movement log the security portal reads.

ALTER TABLE campus_ops.visitor_passes
    ADD COLUMN IF NOT EXISTS entry_mode text NOT NULL DEFAULT 'invitation',
    ADD COLUMN IF NOT EXISTS vehicle_number text,
    ADD COLUMN IF NOT EXISTS id_note text;

ALTER TABLE campus_ops.visitor_passes
    DROP CONSTRAINT IF EXISTS visitor_passes_entry_mode_check;
ALTER TABLE campus_ops.visitor_passes
    ADD CONSTRAINT visitor_passes_entry_mode_check
    CHECK (entry_mode IN ('invitation', 'walk_in'));

ALTER TABLE campus_ops.visitor_passes
    DROP CONSTRAINT IF EXISTS visitor_passes_state_check;
ALTER TABLE campus_ops.visitor_passes
    ADD CONSTRAINT visitor_passes_state_check
    CHECK (state IN (
        'pending_admin', 'approved', 'rejected', 'sent', 'active',
        'checked_in', 'checked_out', 'cancelled', 'expired'
    ));

CREATE INDEX IF NOT EXISTS gate_movements_tenant_created_idx
    ON campus_ops.gate_movements (tenant_id, created_at DESC);

CREATE INDEX IF NOT EXISTS gate_movements_request_idx
    ON campus_ops.gate_movements (tenant_id, request_id, created_at DESC)
    WHERE request_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS gate_movements_member_idx
    ON campus_ops.gate_movements (tenant_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS gatepass_requests_qr_hash_idx
    ON campus_ops.gatepass_requests (tenant_id, qr_token_hash)
    WHERE qr_token_hash IS NOT NULL;
