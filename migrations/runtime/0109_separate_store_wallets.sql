DO $$
DECLARE
    pk_name text;
BEGIN
    SELECT tc.constraint_name INTO pk_name
    FROM information_schema.table_constraints tc
    WHERE tc.table_schema = 'campus_ops'
      AND tc.table_name = 'canteen_wallets'
      AND tc.constraint_type = 'PRIMARY KEY';
      
    IF pk_name IS NOT NULL THEN
        EXECUTE 'ALTER TABLE campus_ops.canteen_wallets DROP CONSTRAINT ' || pk_name;
    END IF;
END $$;
ALTER TABLE campus_ops.canteen_wallets ADD COLUMN shop_key text NOT NULL DEFAULT 'mec-canteen';
ALTER TABLE campus_ops.canteen_wallets ADD PRIMARY KEY (tenant_id, user_id, shop_key);

ALTER TABLE campus_ops.canteen_wallet_transactions ADD COLUMN shop_key text NOT NULL DEFAULT 'mec-canteen';
