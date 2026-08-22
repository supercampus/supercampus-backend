-- The student canteen presents three top-level storefronts — Classic, Bites and
-- Stationery — above the food categories it already had. Stationery is not food:
-- it carries its own sub-categories ("Hair Care & Shampoo", "Soaps & Detergents"),
-- so `category` stops being a fixed food enum and becomes a tenant-defined label
-- scoped to its store.

ALTER TABLE campus_ops.canteen_menu_items
    ADD COLUMN IF NOT EXISTS store text NOT NULL DEFAULT 'classic';

-- Existing rows predate the split: full meals are the Classic counter, and the
-- snacks and drinks that were sold alongside them are Bites.
UPDATE campus_ops.canteen_menu_items
SET store = CASE WHEN category = 'meals' THEN 'classic' ELSE 'bites' END
WHERE store = 'classic' AND category <> 'meals';

ALTER TABLE campus_ops.canteen_menu_items
    DROP CONSTRAINT IF EXISTS canteen_menu_items_store_check;
ALTER TABLE campus_ops.canteen_menu_items
    ADD CONSTRAINT canteen_menu_items_store_check
    CHECK (store IN ('classic', 'bites', 'stationery'));

-- Stationery categories are whatever the owner names them, so the food-only
-- CHECK has to go. The column stays NOT NULL and non-blank: an item still has
-- to sit under some category within its store.
ALTER TABLE campus_ops.canteen_menu_items
    DROP CONSTRAINT IF EXISTS canteen_menu_items_category_check;
ALTER TABLE campus_ops.canteen_menu_items
    ADD CONSTRAINT canteen_menu_items_category_check
    CHECK (length(btrim(category)) > 0);

-- Items marked instant are served straight from the counter with no wait; the
-- menu badges them with a lightning bolt.
ALTER TABLE campus_ops.canteen_menu_items
    ADD COLUMN IF NOT EXISTS is_instant boolean NOT NULL DEFAULT false;

-- The menu is read one storefront at a time.
CREATE INDEX IF NOT EXISTS campus_ops_canteen_menu_store_idx
    ON campus_ops.canteen_menu_items (tenant_id, store, category);

-- Order lines snapshot the item so history survives menu edits, but they never
-- captured whether the item was vegetarian, which the streak screens need to
-- count veg days. Backfill what is still resolvable; anything whose item was
-- already deleted stays absent rather than guessing.
UPDATE campus_ops.canteen_orders AS o
SET lines = (
    SELECT jsonb_agg(
        CASE
            WHEN line ? 'isVegetarian' THEN line
            WHEN item.is_vegetarian IS NULL THEN line
            ELSE line || jsonb_build_object('isVegetarian', item.is_vegetarian)
        END
        ORDER BY ordinality
    )
    FROM jsonb_array_elements(o.lines) WITH ORDINALITY AS entry(line, ordinality)
    LEFT JOIN campus_ops.canteen_menu_items AS item
        ON item.tenant_id = o.tenant_id
        AND item.id = NULLIF(line ->> 'itemId', '')::uuid
)
WHERE jsonb_typeof(o.lines) = 'array'
  AND jsonb_array_length(o.lines) > 0
  AND NOT (o.lines @> '[{"isVegetarian": true}]'::jsonb
        OR o.lines @> '[{"isVegetarian": false}]'::jsonb);
