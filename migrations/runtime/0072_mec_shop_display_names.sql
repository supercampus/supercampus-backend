-- Shop names are tenant-owned. Keep this MEC-specific presentation change out
-- of the application so every other tenant retains its configured names.
UPDATE campus_ops.shops shop
SET name = CASE shop.shop_key
      WHEN 'mec-canteen' THEN 'Canteen'
      WHEN 'mec-laundry' THEN 'Laundry'
      ELSE shop.name
    END,
    description = CASE shop.shop_key
      WHEN 'mec-canteen' THEN 'Canteen at Madras Engineering College'
      WHEN 'mec-laundry' THEN 'Laundry at Madras Engineering College'
      ELSE shop.description
    END,
    updated_at = now()
FROM platform.tenants tenant
WHERE shop.tenant_id = tenant.id
  AND tenant.slug = 'mec'
  AND shop.shop_key IN ('mec-canteen', 'mec-laundry');
