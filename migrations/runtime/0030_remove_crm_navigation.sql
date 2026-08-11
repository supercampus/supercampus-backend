-- CRM operations now live in Overview, Lead, Application Desk, and Settings.
-- Retire the standalone CRM workspace entry for every tenant while preserving
-- the row for auditability and safe rollback through a future forward migration.
UPDATE platform.navigation_sections
SET active = false, updated_at = now()
WHERE section_key = 'crm'
  AND kind = 'workspace';
