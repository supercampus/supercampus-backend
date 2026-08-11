-- Keep persisted tenant navigation labels aligned with the current admissions UI.
-- Section keys remain unchanged so routes, grants, and saved navigation state are stable.
UPDATE platform.navigation_sections
SET label = 'Overview', updated_at = now()
WHERE section_key = 'dashboard'
  AND kind = 'workspace';

UPDATE platform.navigation_sections
SET label = 'Lead', updated_at = now()
WHERE section_key = 'pipeline'
  AND kind = 'workspace';
