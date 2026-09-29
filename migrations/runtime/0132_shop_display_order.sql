-- The administrator's own sequence for campus shops. Every shop listing
-- (storefront tabs, wallets, vendor register, sales, reports) sorts by
-- sort_order, then name; shops never placed (NULL) follow in name order.
-- Idempotent: the API also applies it on first use of a tenant database.
DO $$
BEGIN
    IF to_regclass('campus_ops.shops') IS NOT NULL THEN
        ALTER TABLE campus_ops.shops ADD COLUMN IF NOT EXISTS sort_order integer;
        CREATE INDEX IF NOT EXISTS shops_tenant_sort_order_idx
            ON campus_ops.shops (tenant_id, sort_order, name);
    END IF;
END $$;
