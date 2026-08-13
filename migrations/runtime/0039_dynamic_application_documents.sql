-- Admission Desk documents are now derived exclusively from each applicant's
-- submitted Application form schema snapshot. Remove the legacy static
-- checklist records from existing cases; the application snapshot remains in
-- attributes and is re-extracted by the service on the next application save
-- or accepted-offer handoff.
UPDATE application_desk.cases
SET document = jsonb_set(document, '{documents}', '[]'::jsonb, true),
    updated_at = now()
WHERE document ? 'documents'
  AND document #> '{attributes,applicationForm}' IS NOT NULL;

UPDATE application_desk.workflows
SET definition = jsonb_set(definition, '{documentChecklist}', '[]'::jsonb, true),
    updated_at = now()
WHERE definition ? 'documentChecklist';
