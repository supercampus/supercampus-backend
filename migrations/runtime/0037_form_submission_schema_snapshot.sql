-- Freeze the published form schema with every submission. Form definitions
-- remain editable/versioned, but an admissions reviewer must always see the
-- exact questions that produced the stored answers.

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
