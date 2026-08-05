CREATE SCHEMA IF NOT EXISTS crm;

CREATE TABLE IF NOT EXISTS crm.leads (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    full_name text NOT NULL,
    email text,
    phone text,
    whatsapp text,
    parent_name text,
    parent_phone text,
    source text NOT NULL,
    source_detail jsonb NOT NULL DEFAULT '{}'::jsonb,
    academic jsonb NOT NULL DEFAULT '{}'::jsonb,
    interest jsonb NOT NULL DEFAULT '{}'::jsonb,
    pipeline_key text NOT NULL DEFAULT 'pre-admission',
    stage_key text NOT NULL DEFAULT 'enquiry',
    substate_key text NOT NULL DEFAULT 'new',
    global_status text,
    global_status_data jsonb NOT NULL DEFAULT '{}'::jsonb,
    assigned_to text,
    assigned_by text,
    assignment_type text,
    priority text NOT NULL DEFAULT 'medium',
    follow_up_at timestamptz,
    preferred_channel text,
    consent_given boolean NOT NULL DEFAULT false,
    fee_payment_confirmed boolean NOT NULL DEFAULT false,
    documents_verified boolean NOT NULL DEFAULT false,
    scholarship_status text NOT NULL DEFAULT 'none',
    erp_status text NOT NULL DEFAULT 'not_ready',
    erp_student_id text,
    erp_enrollment_number text,
    duplicate_of uuid REFERENCES crm.leads(id),
    custom_fields jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by text NOT NULL,
    stage_entered_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    CHECK (priority IN ('high', 'medium', 'low', 'urgent')),
    CHECK (global_status IS NULL OR global_status IN ('prospect', 'deferred', 'on_hold', 'archive'))
);

-- Forward-compatible expansion for installations that used the original CRM lead table.
ALTER TABLE crm.leads
    ADD COLUMN IF NOT EXISTS phone text,
    ADD COLUMN IF NOT EXISTS whatsapp text,
    ADD COLUMN IF NOT EXISTS parent_name text,
    ADD COLUMN IF NOT EXISTS parent_phone text,
    ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'Unknown',
    ADD COLUMN IF NOT EXISTS source_detail jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS academic jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS interest jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS substate_key text NOT NULL DEFAULT 'new',
    ADD COLUMN IF NOT EXISTS global_status text,
    ADD COLUMN IF NOT EXISTS global_status_data jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS assigned_to text,
    ADD COLUMN IF NOT EXISTS assigned_by text,
    ADD COLUMN IF NOT EXISTS assignment_type text,
    ADD COLUMN IF NOT EXISTS priority text NOT NULL DEFAULT 'medium',
    ADD COLUMN IF NOT EXISTS follow_up_at timestamptz,
    ADD COLUMN IF NOT EXISTS preferred_channel text,
    ADD COLUMN IF NOT EXISTS consent_given boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS fee_payment_confirmed boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS documents_verified boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS scholarship_status text NOT NULL DEFAULT 'none',
    ADD COLUMN IF NOT EXISTS erp_status text NOT NULL DEFAULT 'not_ready',
    ADD COLUMN IF NOT EXISTS erp_student_id text,
    ADD COLUMN IF NOT EXISTS erp_enrollment_number text,
    ADD COLUMN IF NOT EXISTS duplicate_of uuid REFERENCES crm.leads(id),
    ADD COLUMN IF NOT EXISTS stage_entered_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS deleted_at timestamptz;
ALTER TABLE crm.leads ALTER COLUMN pipeline_key SET DEFAULT 'pre-admission';
ALTER TABLE crm.leads ALTER COLUMN stage_key SET DEFAULT 'enquiry';
ALTER TABLE crm.leads DROP CONSTRAINT IF EXISTS leads_priority_check;
ALTER TABLE crm.leads ADD CONSTRAINT leads_priority_check
    CHECK (priority IN ('high', 'medium', 'low', 'urgent'));
ALTER TABLE crm.leads DROP CONSTRAINT IF EXISTS leads_global_status_check;
ALTER TABLE crm.leads ADD CONSTRAINT leads_global_status_check
    CHECK (global_status IS NULL OR global_status IN ('prospect', 'deferred', 'on_hold', 'archive'));
CREATE INDEX IF NOT EXISTS crm_leads_pipeline_idx
    ON crm.leads (tenant_id, pipeline_key, stage_key, substate_key) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS crm_leads_owner_idx
    ON crm.leads (tenant_id, assigned_to, updated_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS crm_leads_source_idx
    ON crm.leads (tenant_id, source, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS crm_leads_search_idx
    ON crm.leads (tenant_id, lower(full_name), lower(coalesce(email, '')), phone) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS crm.stage_history (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    from_stage text,
    from_substate text,
    to_stage text NOT NULL,
    to_substate text NOT NULL,
    actor_id text NOT NULL,
    actor_role text NOT NULL,
    reason text,
    notes text,
    ip_address text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS crm_stage_history_lead_idx
    ON crm.stage_history (tenant_id, lead_id, created_at DESC);

CREATE TABLE IF NOT EXISTS crm.assignment_history (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    old_owner text,
    new_owner text NOT NULL,
    assignment_type text NOT NULL,
    reason text,
    actor_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS crm.holds (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    reason text NOT NULL,
    hold_until date,
    reminder_date date,
    placed_by text NOT NULL,
    released_by text,
    release_reason text,
    released_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS crm_one_active_hold_idx
    ON crm.holds (tenant_id, lead_id) WHERE released_at IS NULL;

CREATE TABLE IF NOT EXISTS crm.archive_records (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    previous_stage text NOT NULL,
    previous_substate text NOT NULL,
    reason text NOT NULL,
    notes text,
    archived_by text NOT NULL,
    unarchived_by text,
    unarchive_reason text,
    unarchived_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS crm.forms (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    name text NOT NULL,
    form_type text NOT NULL,
    program_id text,
    intake_year integer,
    version integer NOT NULL DEFAULT 1,
    status text NOT NULL DEFAULT 'draft',
    schema jsonb NOT NULL,
    created_by text NOT NULL,
    updated_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    CHECK (status IN ('draft', 'published', 'archived'))
);

CREATE TABLE IF NOT EXISTS crm.form_submissions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    form_id uuid NOT NULL REFERENCES crm.forms(id),
    form_version integer NOT NULL,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    data jsonb NOT NULL,
    submitted_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS crm.communications (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid NOT NULL REFERENCES crm.leads(id) ON DELETE CASCADE,
    channel text NOT NULL,
    direction text NOT NULL DEFAULT 'outbound',
    template_key text,
    subject text,
    content jsonb NOT NULL DEFAULT '{}'::jsonb,
    outcome text,
    status text NOT NULL DEFAULT 'queued',
    actor_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (channel IN ('whatsapp', 'email', 'call', 'sms'))
);

CREATE TABLE IF NOT EXISTS crm.communication_templates (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    template_key text NOT NULL,
    channel text NOT NULL,
    name text NOT NULL,
    content text NOT NULL,
    language text NOT NULL DEFAULT 'en',
    status text NOT NULL DEFAULT 'draft',
    created_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, template_key, language)
);

CREATE TABLE IF NOT EXISTS crm.workflow_toggles (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    from_stage text NOT NULL,
    to_stage text NOT NULL,
    allowed_roles jsonb NOT NULL DEFAULT '[]'::jsonb,
    requires_approval boolean NOT NULL DEFAULT false,
    approval_role text,
    enabled boolean NOT NULL DEFAULT true,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, from_stage, to_stage)
);

CREATE TABLE IF NOT EXISTS crm.automation_toggles (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    stage text NOT NULL,
    trigger_name text NOT NULL,
    action text NOT NULL,
    template_key text,
    conditions jsonb NOT NULL DEFAULT '[]'::jsonb,
    enabled boolean NOT NULL DEFAULT true,
    mandatory boolean NOT NULL DEFAULT false,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, stage, trigger_name, action)
);

CREATE TABLE IF NOT EXISTS crm.counselor_capacity (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    display_name text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    max_capacity integer NOT NULL DEFAULT 100,
    source_categories jsonb NOT NULL DEFAULT '[]'::jsonb,
    program_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
    territories jsonb NOT NULL DEFAULT '[]'::jsonb,
    average_response_minutes double precision NOT NULL DEFAULT 60,
    conversion_rate double precision NOT NULL DEFAULT 0,
    last_assigned_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE TABLE IF NOT EXISTS crm.outbox_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sequence_id bigint GENERATED BY DEFAULT AS IDENTITY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    aggregate_id uuid NOT NULL,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    attempts integer NOT NULL DEFAULT 0,
    available_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);
CREATE INDEX IF NOT EXISTS crm_outbox_pending_idx
    ON crm.outbox_events (status, available_at) WHERE status = 'pending';
CREATE UNIQUE INDEX IF NOT EXISTS crm_outbox_tenant_sequence_idx
    ON crm.outbox_events (tenant_id, sequence_id);

CREATE TABLE IF NOT EXISTS crm.permission_audit (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    actor_id text NOT NULL,
    actor_role text NOT NULL,
    action text NOT NULL,
    entity_type text NOT NULL,
    entity_id text,
    allowed boolean NOT NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT now()
);

DO $$
DECLARE table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'leads', 'stage_history', 'assignment_history', 'holds', 'archive_records',
        'forms', 'form_submissions', 'communications', 'communication_templates',
        'workflow_toggles', 'automation_toggles', 'counselor_capacity',
        'outbox_events', 'permission_audit'
    ]
    LOOP
        EXECUTE format('ALTER TABLE crm.%I ENABLE ROW LEVEL SECURITY', table_name);
        EXECUTE format('DROP POLICY IF EXISTS tenant_isolation ON crm.%I', table_name);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON crm.%I USING (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid) WITH CHECK (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid)',
            table_name
        );
    END LOOP;
END $$;