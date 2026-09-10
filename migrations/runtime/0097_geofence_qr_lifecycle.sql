-- A member keeps one gate-in credential for as long as their device remains
-- inside the configured campus fence. The raw payload is retained so repeated
-- location checks can return the same QR instead of rotating it.
ALTER TABLE campus_ops.daily_access_passes
    ADD COLUMN IF NOT EXISTS qr_payload text;

-- Rows from previous releases expired at midnight and cannot be reconstructed
-- because only their hashes were stored. Remove that legacy history before
-- enforcing one active geofence credential per member.
DELETE FROM campus_ops.daily_access_passes
WHERE valid_on < CURRENT_DATE;

CREATE UNIQUE INDEX IF NOT EXISTS daily_access_active_member_idx
    ON campus_ops.daily_access_passes (tenant_id, user_id);
