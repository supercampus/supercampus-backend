-- Rename the user-facing workspace while preserving the stable module, route,
-- permission, and database identifiers used by deployed clients.
UPDATE platform.navigation_sections
SET label = 'Admission Desk', updated_at = now()
WHERE section_key = 'application-desk'
  AND label IS DISTINCT FROM 'Admission Desk';
