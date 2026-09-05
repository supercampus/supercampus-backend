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

-- The stationery workspace edits price, availability and image metadata through
-- the shared canteen menu endpoint. Keep the grant assignment-scoped so this
-- operator can update only the stationery shop assigned to the account.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, surface, scope, constraints, granted_by)
SELECT role.tenant_id, role.id, permission.permission_key, 'app', 'assigned',
       '{}'::jsonb, 'runtime-migration-0081'
FROM authz.roles role
JOIN authz.permission_definitions permission
  ON permission.tenant_id = role.tenant_id
 AND permission.permission_key = 'canteen.menu.update'
WHERE role.role_key = 'stationery_operator' AND role.active
ON CONFLICT (tenant_id, role_id, surface, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = now();
