-- Enquiry ownership is established only by the first stage movement.
-- Preserve explicit manager assignment/reassignment, but release cards claimed
-- by the retired pre-movement claim and automatic-assignment paths.
UPDATE crm.leads
SET assigned_to = NULL,
    assigned_by = NULL,
    assignment_type = NULL,
    updated_at = now()
WHERE deleted_at IS NULL
  AND stage_key = 'enquiry'
  AND assigned_to IS NOT NULL
  AND (
      assignment_type IN (
          'self_claim',
          'automatic',
          'round_robin',
          'auto_assign_digital_leads'
      )
      OR (
          assignment_type IS NULL
          AND assigned_to = created_by
          AND NOT EXISTS (
              SELECT 1
              FROM crm.assignment_history history
              WHERE history.tenant_id = crm.leads.tenant_id
                AND history.lead_id = crm.leads.id
                AND history.assignment_type IN ('manual', 'reassignment')
          )
      )
  );

-- Retain the endpoint permission for compatibility, but ensure the legacy
-- automation remains disabled for every existing tenant.
UPDATE crm.automation_toggles
SET enabled = false,
    updated_at = now()
WHERE action = 'auto_assign_digital_leads';
