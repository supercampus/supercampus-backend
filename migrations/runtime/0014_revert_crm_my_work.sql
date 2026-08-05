-- Forward-only rollback of the CRM personal workspace introduced by migration 0013.
-- Remove the feature grants and catalog entries before dropping its data structures.
DELETE FROM authz.role_permissions
WHERE permission_key = 'crm.my_work.read';

DELETE FROM authz.permission_definitions
WHERE permission_key = 'crm.my_work.read';

DELETE FROM authz.permission_templates
WHERE permission_key = 'crm.my_work.read';

DROP TABLE IF EXISTS crm.fee_payments;
DROP TABLE IF EXISTS crm.fee_invoices;
DROP TABLE IF EXISTS crm.scholarship_applications;
DROP TABLE IF EXISTS crm.interviews;
DROP TABLE IF EXISTS crm.approval_requests;
DROP TABLE IF EXISTS crm.application_reviews;
DROP TABLE IF EXISTS crm.admission_documents;
DROP TABLE IF EXISTS crm.work_tasks;

ALTER TABLE crm.counselor_capacity
    DROP CONSTRAINT IF EXISTS counselor_capacity_monthly_target_check;

ALTER TABLE crm.counselor_capacity
    DROP COLUMN IF EXISTS monthly_conversion_target;