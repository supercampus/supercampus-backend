-- Attach the MEC captain identities to the live canteen inside the MEC tenant
-- database. Identity and RBAC provisioning remains in runtime migration 0075.
DO $$
DECLARE
    mec_tenant_id uuid;
    canteen_shop_id uuid;
    captain_user_id uuid;
BEGIN
    SELECT id INTO mec_tenant_id
      FROM platform.tenants
     WHERE slug = 'mec';
    IF mec_tenant_id IS NULL THEN
        RETURN;
    END IF;

    SELECT shop.id INTO canteen_shop_id
      FROM campus_ops.shops shop
     WHERE shop.tenant_id = mec_tenant_id
       AND (
         shop.shop_key IN ('mec-canteen', 'classic')
         OR lower(shop.category) = 'canteen'
         OR EXISTS (
           SELECT 1
             FROM campus_ops.canteen_menu_items item
            WHERE item.tenant_id = shop.tenant_id
              AND item.store = shop.shop_key
         )
       )
     ORDER BY CASE
       WHEN shop.shop_key = 'mec-canteen' THEN 0
       WHEN lower(shop.name) = 'canteen' THEN 1
       WHEN shop.shop_key = 'classic' THEN 2
       WHEN lower(shop.category) = 'canteen' THEN 3
       ELSE 4
     END,
     (SELECT count(*)
        FROM campus_ops.canteen_menu_items item
       WHERE item.tenant_id = shop.tenant_id
         AND item.store = shop.shop_key) DESC,
     shop.created_at
     LIMIT 1;
    IF canteen_shop_id IS NULL THEN
        RAISE EXCEPTION 'MEC needs a canteen shop before assigning captains';
    END IF;

    UPDATE campus_ops.shops
       SET is_active = true,
           updated_at = now()
     WHERE tenant_id = mec_tenant_id
       AND id = canteen_shop_id;

    FOREACH captain_user_id IN ARRAY ARRAY[
        '337c8d72-79b0-5dd4-a311-fbd72cbe9f01'::uuid,
        'b0db044a-7a27-5ab6-9e59-d2b3df42b202'::uuid,
        'f235af7d-b947-5d5d-94b4-fc95f6f5d303'::uuid,
        'a3e190a4-4f6f-59f2-b2f1-8768bc2cd404'::uuid
    ]
    LOOP
        INSERT INTO campus_ops.shop_user_assignments
            (tenant_id, shop_id, user_id, assignment_role, is_active,
             assigned_by)
        VALUES
            (mec_tenant_id, canteen_shop_id, captain_user_id::text,
             'captain', true, 'runtime-migration-0076')
        ON CONFLICT (tenant_id, shop_id, user_id) DO UPDATE SET
            assignment_role = 'captain',
            is_active = true,
            assigned_by = 'runtime-migration-0076',
            updated_at = now();

        INSERT INTO campus_ops.canteen_staff_state
            (tenant_id, user_id, mode, shop_open)
        VALUES (mec_tenant_id, captain_user_id::text, 'work', NULL)
        ON CONFLICT (tenant_id, user_id) DO NOTHING;
    END LOOP;
END $$;
