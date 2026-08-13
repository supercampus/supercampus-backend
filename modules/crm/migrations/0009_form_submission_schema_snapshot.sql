-- Module-local copy of runtime migration 0037.
ALTER TABLE crm.form_submissions
    ADD COLUMN IF NOT EXISTS form_schema jsonb;

UPDATE crm.form_submissions AS submission
SET form_schema = form.schema
FROM crm.forms AS form
WHERE form.tenant_id = submission.tenant_id
  AND form.id = submission.form_id
  AND submission.form_schema IS NULL;

ALTER TABLE crm.form_submissions
    ALTER COLUMN form_schema SET NOT NULL;
