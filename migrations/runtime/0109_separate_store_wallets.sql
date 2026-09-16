ALTER TABLE campus_ops.canteen_wallets DROP CONSTRAINT canteen_wallets_pkey;
ALTER TABLE campus_ops.canteen_wallets ADD COLUMN shop_key text NOT NULL DEFAULT 'mec-canteen';
ALTER TABLE campus_ops.canteen_wallets ADD PRIMARY KEY (tenant_id, user_id, shop_key);

ALTER TABLE campus_ops.canteen_wallet_transactions ADD COLUMN shop_key text NOT NULL DEFAULT 'mec-canteen';
