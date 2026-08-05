# Sorting guide

Client-selected sorting is **Not Implemented**.

- CRM leads: `stage_entered_at ASC, created_at ASC`.
- Generic records: `updated_at DESC`.
- Forms: `updated_at DESC`.
- Form submissions: `created_at DESC`.
- Templates: `template_key, language`.
- Workflow toggles: `from_stage, to_stage`.
- Automation toggles: `stage, trigger_name`.
- Timeline history and communications: newest first.

Future sort parameters must use an allowlist and must never interpolate arbitrary SQL identifiers.
