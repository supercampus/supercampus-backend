-- Laundry charge QR lifecycle.
--
-- The counter keeps an unpaid charge's QR so it can show it again until the
-- student pays, and records who voided a charge and when. The API applies the
-- same forward-only change at runtime (ensure_laundry_charge_schema in
-- operations.rs) for databases the migrator cannot reach.
ALTER TABLE campus_ops.laundry_charges
    ADD COLUMN IF NOT EXISTS qr_payload text,
    ADD COLUMN IF NOT EXISTS cancelled_at timestamptz,
    ADD COLUMN IF NOT EXISTS cancelled_by text;
