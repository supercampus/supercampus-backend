-- Keep tenant provisioning idempotent so a deployment never creates a duplicate.
INSERT INTO platform.tenants (slug, code, name, city, status)
VALUES ('mec', 'MEC', 'Madras Engineering College', 'Chennai', 'active')
ON CONFLICT (slug) DO UPDATE
SET code = EXCLUDED.code,
    name = EXCLUDED.name,
    city = EXCLUDED.city,
    status = EXCLUDED.status,
    updated_at = now();

-- MEC is the sole live tenant. Preserve any historical tenant rows and their
-- related records, but prevent those tenants from authenticating.
UPDATE platform.tenants
SET status = 'inactive',
    updated_at = now()
WHERE slug <> 'mec'
  AND status <> 'inactive';
