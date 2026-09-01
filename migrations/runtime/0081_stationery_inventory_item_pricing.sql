ALTER TABLE campus_ops.canteen_menu_items
    ADD COLUMN IF NOT EXISTS actual_price numeric(12,2);

UPDATE campus_ops.canteen_menu_items
SET actual_price = price
WHERE actual_price IS NULL;

ALTER TABLE campus_ops.canteen_menu_items
    ALTER COLUMN actual_price SET NOT NULL;

ALTER TABLE campus_ops.canteen_menu_items
    DROP CONSTRAINT IF EXISTS canteen_menu_items_actual_price_check;

ALTER TABLE campus_ops.canteen_menu_items
    ADD CONSTRAINT canteen_menu_items_actual_price_check
    CHECK (actual_price >= 0);
