-- Who did what to each canteen order, item by item.
--
-- campus_ops.canteen_orders.handled_by only remembers the LAST account that
-- moved an order, so it cannot say which captain prepared or handed over
-- which item. Every status change at the counter now writes one row here per
-- item it moved (line_index = the item's position in the order's lines), or
-- one whole-order row (line_index NULL) for accepting or rejecting an order:
--
--   action      the step the item (or order) reached: accepted, preparing,
--               ready, delivered, rejected, cancelled
--   source      how: 'item' (a swipe on one item), 'order' (the whole order
--               moved at once) or 'scan' (the pickup QR)
--   actor_*     the account that did it, and its name at the time
--   quantity    the item's quantity (the order's item count for whole-order
--               rows)
--   amount      the item's line total (price x quantity) for item rows; the
--               refunded total for a rejection
--   instant     whether the item is handed over without kitchen preparation
--
-- Orders moved before this table existed have no rows; the owner's figures
-- show them as "before tracking" rather than guessing who handled them.
--
-- Idempotent: the API also applies it on first use of a tenant database.
DO $$
BEGIN
    IF to_regclass('campus_ops.canteen_orders') IS NOT NULL THEN
        CREATE TABLE IF NOT EXISTS campus_ops.canteen_order_events (
            id bigserial PRIMARY KEY,
            tenant_id uuid NOT NULL,
            order_id uuid NOT NULL,
            line_index integer,
            item_id text,
            item_name text,
            action text NOT NULL CHECK (action IN
                ('accepted','preparing','ready','delivered','rejected','cancelled')),
            source text NOT NULL DEFAULT 'order' CHECK (source IN ('item','order','scan')),
            actor_user_id text NOT NULL,
            actor_name text,
            shop_key text NOT NULL,
            quantity integer NOT NULL DEFAULT 0,
            amount numeric(12,2) NOT NULL DEFAULT 0,
            instant boolean NOT NULL DEFAULT false,
            occurred_at timestamptz NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS canteen_order_events_shop_time_idx
            ON campus_ops.canteen_order_events (tenant_id, shop_key, occurred_at);
        CREATE INDEX IF NOT EXISTS canteen_order_events_order_idx
            ON campus_ops.canteen_order_events (tenant_id, order_id, line_index);
        CREATE INDEX IF NOT EXISTS canteen_order_events_actor_idx
            ON campus_ops.canteen_order_events (tenant_id, actor_user_id, occurred_at);
    END IF;
END $$;
