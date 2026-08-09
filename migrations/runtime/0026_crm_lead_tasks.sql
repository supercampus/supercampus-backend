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
CREATE INDEX IF NOT EXISTS crm_work_tasks_owner_due_idx
    ON crm.work_tasks (tenant_id, assigned_to, due_at) WHERE status <> 'completed';
ALTER TABLE crm.work_tasks ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS tenant_isolation ON crm.work_tasks;
CREATE POLICY tenant_isolation ON crm.work_tasks
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
