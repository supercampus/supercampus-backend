-- CRM stores only the tenant-scoped relationship to the authoritative
-- Application Desk case. Application identifiers, status, and history remain
-- owned by application_desk.cases / application_desk.audit_log.
ALTER TABLE crm.lead_application_links
    DROP CONSTRAINT IF EXISTS lead_application_links_pkey,
    DROP CONSTRAINT IF EXISTS lead_application_links_tenant_id_case_id_key,
    DROP COLUMN IF EXISTS application_id,
    DROP COLUMN IF EXISTS admission_id,
    DROP COLUMN IF EXISTS application_status;

ALTER TABLE crm.lead_application_links
    ADD CONSTRAINT lead_application_links_pkey PRIMARY KEY (tenant_id, case_id);

CREATE INDEX IF NOT EXISTS lead_application_links_lead_idx
    ON crm.lead_application_links (tenant_id, lead_id, created_at DESC);

-- Composite foreign keys make it impossible to connect a lead or case from a
-- different tenant even if code outside the API writes the table directly.
CREATE UNIQUE INDEX IF NOT EXISTS crm_leads_tenant_id_id_uidx
    ON crm.leads (tenant_id, id);

ALTER TABLE crm.lead_application_links
    DROP CONSTRAINT IF EXISTS lead_application_links_lead_id_fkey;

ALTER TABLE crm.lead_application_links
    ADD CONSTRAINT lead_application_links_tenant_lead_fk
        FOREIGN KEY (tenant_id, lead_id)
        REFERENCES crm.leads (tenant_id, id)
        ON DELETE CASCADE,
    ADD CONSTRAINT lead_application_links_tenant_case_fk
        FOREIGN KEY (tenant_id, case_id)
        REFERENCES application_desk.cases (tenant_id, id)
        ON DELETE CASCADE;
