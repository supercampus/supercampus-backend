-- Counter availability belongs to the shop, not to whichever operator last
-- used a device. Students and order creation therefore read one shared state.
ALTER TABLE campus_ops.shops
    ADD COLUMN IF NOT EXISTS shop_open boolean NOT NULL DEFAULT true;

CREATE INDEX IF NOT EXISTS shops_tenant_open_idx
    ON campus_ops.shops (tenant_id, is_active, shop_open, name);
