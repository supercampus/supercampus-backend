-- A cart that holds items from more than one shop is split at checkout into one
-- order per shop, each with its own QR, so an order already belongs to exactly
-- one storefront. The column recording which one was never added: 0054 gave the
-- store dimension to menu items and only backfilled `lines` on orders.
--
-- Everything that filters orders by shop therefore fails to plan, not at
-- runtime under some condition but on every call — the canteen store payload,
-- the counter's order list, and the per-shop analytics all reference
-- `canteen_orders.store`.

ALTER TABLE campus_ops.canteen_orders
    ADD COLUMN IF NOT EXISTS store text NOT NULL DEFAULT 'classic';

-- No guessing is needed to fill it in. Each line snapshots the store it was
-- ordered from, and an order's lines all come from one shop by construction, so
-- the first line is the order's store. Orders whose lines predate that snapshot
-- keep the default rather than being assigned a shop they may not belong to.
UPDATE campus_ops.canteen_orders
SET store = lines -> 0 ->> 'store'
WHERE jsonb_typeof(lines) = 'array'
  AND jsonb_array_length(lines) > 0
  AND COALESCE(lines -> 0 ->> 'store', '') <> ''
  AND lines -> 0 ->> 'store' <> store;

-- The shop filter is the access boundary for a vendor's own order list, so it
-- runs on every read a counter makes.
CREATE INDEX IF NOT EXISTS campus_ops_canteen_orders_store_idx
    ON campus_ops.canteen_orders (tenant_id, store);
