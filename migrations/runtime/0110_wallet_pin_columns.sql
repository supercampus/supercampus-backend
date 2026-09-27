-- Wallet PIN storage. The API has read and written these columns for a while,
-- but no migration created them; a database that already has them (added by
-- hand) is left untouched.
DO $$
BEGIN
    IF to_regclass('campus_ops.canteen_wallets') IS NOT NULL THEN
        ALTER TABLE campus_ops.canteen_wallets
            ADD COLUMN IF NOT EXISTS wallet_pin_hash text,
            ADD COLUMN IF NOT EXISTS wallet_pin_hint text;
    END IF;
END $$;
