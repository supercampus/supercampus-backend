-- Visitor invitation lifecycle states and scan logging
ALTER TABLE campus_ops.visitor_passes
    DROP CONSTRAINT IF EXISTS visitor_passes_state_check;

ALTER TABLE campus_ops.visitor_passes
    ADD CONSTRAINT visitor_passes_state_check
    CHECK (state IN (
        'pending_admin', 'approved', 'rejected', 'sent', 'active',
        'checked_in', 'checked_out', 'cancelled', 'expired'
    ));

ALTER TABLE campus_ops.visitor_passes
    ADD COLUMN IF NOT EXISTS relationship text,
    ADD COLUMN IF NOT EXISTS checked_in_at timestamptz,
    ADD COLUMN IF NOT EXISTS checked_out_at timestamptz;

CREATE INDEX IF NOT EXISTS visitor_passes_checked_in_idx
    ON campus_ops.visitor_passes (tenant_id, checked_in_at DESC)
    WHERE checked_in_at IS NOT NULL;
