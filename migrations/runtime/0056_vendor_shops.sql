CREATE TABLE IF NOT EXISTS campus_ops.shops (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    shop_key text NOT NULL,
    name text NOT NULL,
    category text NOT NULL DEFAULT 'Canteen',
    description text NOT NULL DEFAULT '',
    is_active boolean NOT NULL DEFAULT true,
    meal_compliance boolean NOT NULL DEFAULT false,
    qr_payments boolean NOT NULL DEFAULT true,
    created_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT shops_key_format CHECK (shop_key ~ '^[a-z0-9][a-z0-9_-]{1,63}$'),
    CONSTRAINT shops_tenant_key_unique UNIQUE (tenant_id, shop_key)
);

CREATE INDEX IF NOT EXISTS shops_tenant_active_idx
    ON campus_ops.shops (tenant_id, is_active, name);

INSERT INTO campus_ops.shops (tenant_id, shop_key, name, category)
SELECT DISTINCT tenant_id, store,
    CASE store
      WHEN 'classic' THEN 'Campus Classic'
      WHEN 'bites' THEN 'Quick Bites'
      WHEN 'stationery' THEN 'Stationery Store'
      ELSE initcap(replace(store, '_', ' '))
    END,
    CASE WHEN store = 'stationery' THEN 'Stationery' ELSE 'Canteen' END
FROM campus_ops.canteen_menu_items
ON CONFLICT (tenant_id, shop_key) DO NOTHING;

INSERT INTO campus_ops.shops (tenant_id, shop_key, name, category)
SELECT tenant.id, seed.shop_key, seed.name, seed.category
FROM platform.tenants tenant
CROSS JOIN (VALUES
    ('classic', 'Campus Classic', 'Canteen'),
    ('bites', 'Quick Bites', 'Canteen'),
    ('stationery', 'Stationery Store', 'Stationery')
) AS seed(shop_key, name, category)
ON CONFLICT (tenant_id, shop_key) DO NOTHING;

ALTER TABLE campus_ops.canteen_menu_items
    DROP CONSTRAINT IF EXISTS canteen_menu_items_store_check;

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES
    ('vendor_management.vendors.delete', 'vendor_management', 'vendors', 'delete',
     ARRAY['delete']::text[], 'Delete shop',
     'Deactivate a tenant shop while retaining its audit history', true)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name,
       template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key = 'vendor_management.vendors.delete'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();
