-- Personal CRM workspace metadata and queues. No sample business records are
-- inserted: each tenant sees only data produced by its operational flows.
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
VALUES
    ('crm.my_work.read', 'crm', 'my_work', 'read', ARRAY['read']::text[],
     'View personal workspace', 'View the role-aware My Work workspace', true)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions,
     display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name,
       template.description, true
FROM platform.tenants AS tenant
JOIN authz.permission_templates AS template
  ON template.permission_key = 'crm.my_work.read'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- Preserve access for existing CRM readers while still allowing tenant admins
-- to revoke this permission from non-protected roles later.
INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT existing_grant.tenant_id, existing_grant.role_id, 'crm.my_work.read', existing_grant.scope,
       '{}'::jsonb, existing_grant.granted_by
FROM authz.role_permissions AS existing_grant
WHERE existing_grant.permission_key IN ('crm.dashboard.read', 'crm.leads.read')
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT role.tenant_id, role.id, 'crm.my_work.read', 'all', '{}'::jsonb,
       'runtime-migration-0013'
FROM authz.roles AS role
WHERE role.role_key = 'tenant_admin'
ON CONFLICT (tenant_id, role_id, permission_key) DO NOTHING;

CREATE TABLE IF NOT EXISTS crm.work_tasks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    assigned_to text NOT NULL,
    title text NOT NULL,
    task_type text NOT NULL DEFAULT 'follow_up',
    priority text NOT NULL DEFAULT 'medium',
    due_at timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (priority IN ('low', 'medium', 'high', 'urgent')),
    CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled'))
);

CREATE TABLE IF NOT EXISTS crm.admission_documents (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    candidate_name text NOT NULL,
    document_name text NOT NULL,
    assigned_to text,
    status text NOT NULL DEFAULT 'pending_verification',
    submitted_at timestamptz NOT NULL DEFAULT now(),
    reviewed_by text,
    reviewed_at timestamptz,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CHECK (status IN ('pending_verification', 'verified', 'rejected'))
);

CREATE TABLE IF NOT EXISTS crm.application_reviews (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    candidate_name text NOT NULL,
    program_name text,
    assigned_to text,
    status text NOT NULL DEFAULT 'pending_review',
    submitted_at timestamptz NOT NULL DEFAULT now(),
    processed_by text,
    processed_at timestamptz,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CHECK (status IN ('pending_review', 'approved', 'rejected', 'needs_information'))
);

CREATE TABLE IF NOT EXISTS crm.approval_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    candidate_name text NOT NULL,
    request_type text NOT NULL,
    amount numeric(14,2),
    requested_by text NOT NULL,
    assigned_to text,
    status text NOT NULL DEFAULT 'pending',
    requested_at timestamptz NOT NULL DEFAULT now(),
    due_at timestamptz,
    decided_by text,
    decided_at timestamptz,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CHECK (amount IS NULL OR amount >= 0),
    CHECK (status IN ('pending', 'approved', 'rejected', 'needs_information'))
);

CREATE TABLE IF NOT EXISTS crm.fee_invoices (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE SET NULL,
    student_name text NOT NULL,
    program_name text,
    batch_name text,
    amount numeric(14,2) NOT NULL,
    amount_paid numeric(14,2) NOT NULL DEFAULT 0,
    due_date date NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (amount >= 0 AND amount_paid >= 0 AND amount_paid <= amount),
    CHECK (status IN ('pending', 'partially_paid', 'paid', 'cancelled', 'refunded'))
);

CREATE TABLE IF NOT EXISTS crm.fee_payments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    invoice_id uuid NOT NULL REFERENCES crm.fee_invoices(id) ON DELETE CASCADE,
    student_name text NOT NULL,
    mode text NOT NULL,
    amount numeric(14,2) NOT NULL,
    receipt_no text NOT NULL,
    received_by text NOT NULL,
    paid_at timestamptz NOT NULL DEFAULT now(),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CHECK (amount > 0),
    UNIQUE (tenant_id, receipt_no)
);

CREATE TABLE IF NOT EXISTS crm.interviews (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    candidate_name text NOT NULL,
    program_name text,
    assigned_to text NOT NULL,
    scheduled_at timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'scheduled',
    score numeric(5,2),
    notes text,
    completed_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (score IS NULL OR (score >= 0 AND score <= 100)),
    CHECK (status IN ('scheduled', 'completed', 'scored', 'cancelled', 'no_show'))
);

CREATE TABLE IF NOT EXISTS crm.scholarship_applications (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    lead_id uuid REFERENCES crm.leads(id) ON DELETE CASCADE,
    candidate_name text NOT NULL,
    program_name text,
    requested_amount numeric(14,2),
    assigned_to text,
    status text NOT NULL DEFAULT 'pending_verification',
    submitted_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CHECK (requested_amount IS NULL OR requested_amount >= 0),
    CHECK (status IN (
        'pending_verification', 'pending_approval', 'approved', 'rejected',
        'needs_information'
    ))
);

ALTER TABLE crm.counselor_capacity
    ADD COLUMN IF NOT EXISTS monthly_conversion_target integer NOT NULL DEFAULT 0;

ALTER TABLE crm.counselor_capacity
    DROP CONSTRAINT IF EXISTS counselor_capacity_monthly_target_check;

ALTER TABLE crm.counselor_capacity
    ADD CONSTRAINT counselor_capacity_monthly_target_check
    CHECK (monthly_conversion_target >= 0);

CREATE INDEX IF NOT EXISTS crm_work_tasks_owner_due_idx
    ON crm.work_tasks (tenant_id, assigned_to, due_at) WHERE status <> 'completed';
CREATE INDEX IF NOT EXISTS crm_documents_queue_idx
    ON crm.admission_documents (tenant_id, status, assigned_to, submitted_at);
CREATE INDEX IF NOT EXISTS crm_application_reviews_queue_idx
    ON crm.application_reviews (tenant_id, status, assigned_to, submitted_at);
CREATE INDEX IF NOT EXISTS crm_approval_queue_idx
    ON crm.approval_requests (tenant_id, status, assigned_to, requested_at);
CREATE INDEX IF NOT EXISTS crm_fee_invoices_pending_idx
    ON crm.fee_invoices (tenant_id, status, due_date);
CREATE INDEX IF NOT EXISTS crm_fee_payments_today_idx
    ON crm.fee_payments (tenant_id, paid_at);
CREATE INDEX IF NOT EXISTS crm_interviews_assignee_idx
    ON crm.interviews (tenant_id, assigned_to, scheduled_at);
CREATE INDEX IF NOT EXISTS crm_scholarships_queue_idx
    ON crm.scholarship_applications (tenant_id, status, assigned_to, submitted_at);

DO $$
DECLARE table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'work_tasks', 'admission_documents', 'application_reviews',
        'approval_requests', 'fee_invoices', 'fee_payments', 'interviews',
        'scholarship_applications'
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
