-- Counters under a parent canteen, and canteen wallet credit scoped to them.
--
-- A counter is an ordinary shop (its own staff, menu, order queue, scan,
-- figures) whose parent_shop_key names the canteen it belongs to. The parent
-- is what students see as "the canteen" and what the wallet is held at.
--
-- Wallet buckets reuse campus_ops.canteen_wallets rows, one per
-- (tenant, user, shop_key):
--   * the parent canteen's own row is the GENERAL bucket, spendable at any
--     of its counters (existing canteen balances are exactly this, so legacy
--     data needs no conversion);
--   * a counter's own row is credit RESTRICTED to that counter.
-- An order at a counter takes the counter's restricted credit first, then
-- the canteen's general credit.
--
-- Ledger rows record which bucket they touched (shop_key, as before), the
-- scope that bucket stands for (wallet_scope: 'all' or the counter key) and,
-- for purchases and refunds, the counter the order was placed at
-- (counter_shop_key) so a counter's sales include what general credit paid.
-- Orders keep the split they were paid with (wallet_split) so a refund goes
-- back to the buckets it came from.
--
-- Idempotent: the API also applies it on first use of a tenant database.
DO $$
BEGIN
    IF to_regclass('campus_ops.shops') IS NOT NULL THEN
        ALTER TABLE campus_ops.shops ADD COLUMN IF NOT EXISTS parent_shop_key text;
        CREATE INDEX IF NOT EXISTS shops_tenant_parent_idx
            ON campus_ops.shops (tenant_id, parent_shop_key)
            WHERE parent_shop_key IS NOT NULL;
    END IF;
    IF to_regclass('campus_ops.canteen_wallet_transactions') IS NOT NULL THEN
        ALTER TABLE campus_ops.canteen_wallet_transactions
            ADD COLUMN IF NOT EXISTS wallet_scope text;
        ALTER TABLE campus_ops.canteen_wallet_transactions
            ADD COLUMN IF NOT EXISTS counter_shop_key text;
    END IF;
    IF to_regclass('campus_ops.canteen_orders') IS NOT NULL THEN
        ALTER TABLE campus_ops.canteen_orders ADD COLUMN IF NOT EXISTS wallet_split jsonb;
    END IF;
END $$;
