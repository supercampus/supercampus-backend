-- Add missing columns to campus_ops.visitor_passes that the application code
-- already references: relationship (freeform text describing how the visitor
-- relates to the host), checked_in_at / checked_out_at (gate timestamps), and
-- relax the state CHECK to include 'cancelled' / 'checked_in' / 'checked_out'.

ALTER TABLE campus_ops.visitor_passes
    ADD COLUMN IF NOT EXISTS relationship text,
    ADD COLUMN IF NOT EXISTS checked_in_at timestamptz,
    ADD COLUMN IF NOT EXISTS checked_out_at timestamptz;

-- Widen the allowed states. The original constraint only permitted
-- ('pending_admin', 'approved', 'rejected'). The application now also uses
-- 'cancelled', 'checked_in' and 'checked_out'.
ALTER TABLE campus_ops.visitor_passes
    DROP CONSTRAINT IF EXISTS visitor_passes_state_check;

ALTER TABLE campus_ops.visitor_passes
    ADD CONSTRAINT visitor_passes_state_check
        CHECK (state IN ('pending_admin', 'approved', 'rejected', 'cancelled', 'checked_in', 'checked_out'));
