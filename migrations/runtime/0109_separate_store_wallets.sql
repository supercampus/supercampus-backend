DO $$
DECLARE
    pk_name text;
BEGIN
    -- Check if primary key has 3 columns (which means our new PK is already applied)
    IF EXISTS (
        SELECT 1
        FROM information_schema.key_column_usage
        WHERE table_schema = 'campus_ops' AND table_name = 'canteen_wallets'
        GROUP BY constraint_name
        HAVING count(column_name) = 3
    ) THEN
        RETURN; -- Already applied
    END IF;

    -- Drop the old primary key
    SELECT tc.constraint_name INTO pk_name
    FROM information_schema.table_constraints tc
    WHERE tc.table_schema = 'campus_ops'
      AND tc.table_name = 'canteen_wallets'
      AND tc.constraint_type = 'PRIMARY KEY';
      
    IF pk_name IS NOT NULL THEN
        EXECUTE 'ALTER TABLE campus_ops.canteen_wallets DROP CONSTRAINT ' || pk_name;
    END IF;
END $$;

ALTER TABLE campus_ops.canteen_wallets ADD COLUMN IF NOT EXISTS shop_key text NOT NULL DEFAULT 'mec-canteen';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints tc
        WHERE tc.table_schema = 'campus_ops'
          AND tc.table_name = 'canteen_wallets'
          AND tc.constraint_type = 'PRIMARY KEY'
    ) THEN
        ALTER TABLE campus_ops.canteen_wallets ADD PRIMARY KEY (tenant_id, user_id, shop_key);
    END IF;
END $$;

ALTER TABLE campus_ops.canteen_wallet_transactions ADD COLUMN IF NOT EXISTS shop_key text NOT NULL DEFAULT 'mec-canteen';
