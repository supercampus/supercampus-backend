-- The first MEC geofence save predated the campus configuration API and left
-- the active campus centred in eastern Chennai rather than at the institution's
-- official Vellarai location.  Keep this correction deliberately narrow: only
-- the MEC tenant and only the known obsolete coordinate are eligible.
UPDATE core.campuses AS campus
SET metadata = jsonb_set(
    COALESCE(campus.metadata, '{}'::jsonb),
    '{geofence}',
    COALESCE(campus.metadata -> 'geofence', '{}'::jsonb)
        || jsonb_build_object(
            'latitude', 12.9277504,
            'longitude', 79.9926235
        ),
    true
)
FROM platform.tenants AS tenant
WHERE campus.tenant_id = tenant.id
  AND tenant.slug = 'mec'
  AND campus.active
  AND abs((campus.metadata -> 'geofence' ->> 'latitude')::double precision - 13.0104) < 0.000001
  AND abs((campus.metadata -> 'geofence' ->> 'longitude')::double precision - 80.2356) < 0.000001;
