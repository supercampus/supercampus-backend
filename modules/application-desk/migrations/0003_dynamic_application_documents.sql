-- Keep module-local installations aligned with the runtime migration.
UPDATE application_desk.cases
SET document = jsonb_set(document, '{documents}', '[]'::jsonb, true),
    updated_at = now()
WHERE document ? 'documents'
  AND document #> '{attributes,applicationForm}' IS NOT NULL;

UPDATE application_desk.workflows
SET definition = jsonb_set(definition, '{documentChecklist}', '[]'::jsonb, true),
    updated_at = now()
WHERE definition ? 'documentChecklist';
