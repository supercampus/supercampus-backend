-- Application Desk — onboarding orchestration.
--
-- The desk owns exactly one entity, the onboarding case, and references
-- everything else by id. The Student Master lives in `core` because it is owned
-- by Core Administration, not by this module: the desk only requests its
-- creation and stores the returned id.

CREATE SCHEMA IF NOT EXISTS application_desk;
CREATE SCHEMA IF NOT EXISTS core;

-- -- Student Master ---------------------------------------------------------
-- Owned by Core Administration. Created on request from the desk once approval
-- has succeeded; never written to directly by workflow code.
CREATE TABLE IF NOT EXISTS core.students (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_number text NOT NULL,
    full_name text NOT NULL DEFAULT '',
    email text,
    phone text,
    applicant_id text NOT NULL,
    application_id text NOT NULL,
    admission_id text NOT NULL,
    campus_id text,
    department_id text,
    program_id text,
    batch_id text,
    section_id text,
    academic_year text,
    admission_category text,
    user_account_id uuid REFERENCES identity.users(id) ON DELETE SET NULL,
    status text NOT NULL DEFAULT 'active',
    profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- A student number is the institutional identifier; it must be unique per tenant.
CREATE UNIQUE INDEX IF NOT EXISTS students_tenant_number_idx
    ON core.students (tenant_id, student_number);

-- Defence in depth behind the effect ledger: even if idempotency were bypassed,
-- one admission can only ever produce one student.
CREATE UNIQUE INDEX IF NOT EXISTS students_tenant_admission_idx
    ON core.students (tenant_id, admission_id);

-- -- Workflow configuration --------------------------------------------------
-- Stages, conditions, transitions, checklist and approval chain are data.
-- Cases pin (workflow_id, workflow_version) so a mid-flight config change can
-- never rewrite the history of a case already in progress.
CREATE TABLE IF NOT EXISTS application_desk.workflows (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    workflow_id text NOT NULL,
    version integer NOT NULL,
    name text NOT NULL,
    definition jsonb NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, workflow_id, version)
);

CREATE INDEX IF NOT EXISTS workflows_active_idx
    ON application_desk.workflows (tenant_id, active, version DESC);

-- Per-tenant desk settings: intake trigger and student-number format.
CREATE TABLE IF NOT EXISTS application_desk.settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    intake_mode text NOT NULL DEFAULT 'on_confirmed'
        CHECK (intake_mode IN ('on_confirmed', 'on_fee_paid', 'manual')),
    number_format jsonb NOT NULL DEFAULT
        '{"pattern":["year","department","sequence"],"separator":"","sequenceWidth":3}'::jsonb,
    student_role text NOT NULL DEFAULT 'student',
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- -- Onboarding cases --------------------------------------------------------
-- `document` holds the full serialized case; the promoted columns exist for
-- querying, tenant scoping and the duplicate-protection indexes below.
CREATE TABLE IF NOT EXISTS application_desk.cases (
    id text NOT NULL,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    applicant_id text NOT NULL,
    application_id text NOT NULL,
    admission_id text NOT NULL,
    stage text NOT NULL,
    status text NOT NULL,
    resume_stage text,
    workflow_id text NOT NULL,
    workflow_version integer NOT NULL,
    assigned_to text,
    student_number text,
    student_id uuid,
    user_account_id uuid,
    document jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS cases_tenant_queue_idx
    ON application_desk.cases (tenant_id, status, stage, updated_at DESC);

-- Duplicate protection. A live case claims its applicant/application/admission;
-- a closed case (rejected/cancelled/withdrawn/expired) releases the claim so a
-- legitimate re-admission is not blocked.
CREATE UNIQUE INDEX IF NOT EXISTS cases_live_applicant_idx
    ON application_desk.cases (tenant_id, applicant_id)
    WHERE status NOT IN ('REJECTED', 'CANCELLED', 'WITHDRAWN', 'EXPIRED');

CREATE UNIQUE INDEX IF NOT EXISTS cases_live_application_idx
    ON application_desk.cases (tenant_id, application_id)
    WHERE status NOT IN ('REJECTED', 'CANCELLED', 'WITHDRAWN', 'EXPIRED');

CREATE UNIQUE INDEX IF NOT EXISTS cases_live_admission_idx
    ON application_desk.cases (tenant_id, admission_id)
    WHERE status NOT IN ('REJECTED', 'CANCELLED', 'WITHDRAWN', 'EXPIRED');

-- -- Idempotency ledger ------------------------------------------------------
-- Every side effect is keyed <case_id>:<effect>. The UNIQUE constraint is what
-- makes a replayed transition reuse the original student number, student id and
-- account id instead of creating a second student.
--
-- The key carries tenant_id as well. Case references restart per tenant, so
-- `ONB-2026-000001` exists in every institution; a constraint on
-- (case_id, effect) alone would let one tenant's ledger row suppress another
-- tenant's, and the second institution's effects would silently go unrecorded.
CREATE TABLE IF NOT EXISTS application_desk.onboarding_effect (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    case_id text NOT NULL,
    effect text NOT NULL,
    result text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT onboarding_effect_case_effect_key UNIQUE (tenant_id, case_id, effect)
);

CREATE INDEX IF NOT EXISTS onboarding_effect_case_idx
    ON application_desk.onboarding_effect (tenant_id, case_id);

-- -- Transactional student numbering ----------------------------------------
-- Sequences are scoped by (tenant, year, department). Allocation is an atomic
-- upsert, so two operators activating simultaneously can never be handed the
-- same number.
CREATE TABLE IF NOT EXISTS application_desk.number_sequences (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    scope text NOT NULL,
    next_value bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, scope)
);

-- -- Append-only audit -------------------------------------------------------
-- Written in the same transaction as the state change it describes.
CREATE TABLE IF NOT EXISTS application_desk.audit_log (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    case_id text NOT NULL,
    actor text NOT NULL,
    action text NOT NULL,
    from_stage text NOT NULL,
    to_stage text NOT NULL,
    from_status text NOT NULL,
    to_status text NOT NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_log_case_idx
    ON application_desk.audit_log (tenant_id, case_id, id DESC);

-- Enforce append-only at the database, not merely by convention.
CREATE OR REPLACE FUNCTION application_desk.reject_audit_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'application_desk.audit_log is append-only';
END;
$$;

DROP TRIGGER IF EXISTS audit_log_append_only ON application_desk.audit_log;
CREATE TRIGGER audit_log_append_only
    BEFORE UPDATE OR DELETE ON application_desk.audit_log
    FOR EACH ROW EXECUTE FUNCTION application_desk.reject_audit_mutation();

-- -- Transactional outbox ----------------------------------------------------
-- Events are written in the state-change transaction and published after
-- commit. Downstream modules subscribe; the desk never calls them inline.
CREATE TABLE IF NOT EXISTS application_desk.outbox_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sequence_id bigint GENERATED BY DEFAULT AS IDENTITY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    aggregate_id text NOT NULL,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    attempts integer NOT NULL DEFAULT 0,
    available_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);

CREATE INDEX IF NOT EXISTS application_desk_outbox_pending_idx
    ON application_desk.outbox_events (status, available_at) WHERE status = 'pending';

CREATE UNIQUE INDEX IF NOT EXISTS application_desk_outbox_tenant_sequence_idx
    ON application_desk.outbox_events (tenant_id, sequence_id);

-- -- Row level security ------------------------------------------------------
-- Every table carries tenant_id and is isolated by the request's app.tenant_id.
DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'application_desk.workflows', 'application_desk.settings',
        'application_desk.cases', 'application_desk.onboarding_effect',
        'application_desk.number_sequences', 'application_desk.audit_log',
        'application_desk.outbox_events', 'core.students'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', target);
        EXECUTE format('DROP POLICY IF EXISTS tenant_isolation ON %s', target);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %s USING (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid) WITH CHECK (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid)',
            target
        );
    END LOOP;
END;
$$;

-- -- Permission catalogue ----------------------------------------------------
INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description)
VALUES
    ('application-desk.view', 'application-desk', 'cases', 'view', 'View onboarding cases', 'Read the onboarding queue and case detail'),
    ('application-desk.create', 'application-desk', 'cases', 'create', 'Open onboarding cases', 'Open a case for a confirmed admission'),
    ('application-desk.edit', 'application-desk', 'cases', 'edit', 'Advance onboarding cases', 'Move a case forward through the workflow'),
    ('application-desk.verify', 'application-desk', 'verification', 'verify', 'Verify identity and documents', 'Record identity and document verification outcomes'),
    ('application-desk.assign', 'application-desk', 'allocation', 'assign', 'Allocate academic structure', 'Assign section, batch and case owner'),
    ('application-desk.approve', 'application-desk', 'approval', 'approve', 'Approve onboarding', 'Record an approval step'),
    ('application-desk.reject', 'application-desk', 'approval', 'reject', 'Reject onboarding', 'Reject, cancel or withdraw a case'),
    ('application-desk.hold', 'application-desk', 'lifecycle', 'hold', 'Hold onboarding cases', 'Place a case on hold with a reason'),
    ('application-desk.resume', 'application-desk', 'lifecycle', 'resume', 'Resume onboarding cases', 'Resume a held or returned case'),
    ('application-desk.activate', 'application-desk', 'provisioning', 'activate', 'Activate students', 'Run student, account and access provisioning'),
    ('application-desk.manage_settings', 'application-desk', 'configuration', 'manage', 'Manage desk configuration', 'Edit workflow, checklist and intake settings'),
    ('application-desk.reports.read', 'application-desk', 'reports', 'read', 'Read onboarding reports', 'Read desk metrics and queue analytics')
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
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
CROSS JOIN authz.permission_templates AS template
WHERE template.module_key = 'application-desk'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();

-- The desk is a first-class module for the registry and navigation. The module
-- registry is migrated separately, so only register when its table is present.
DO $$
BEGIN
    IF to_regclass('module_registry.installations') IS NOT NULL THEN
        INSERT INTO module_registry.installations
            (tenant_id, module_key, installed_version, status)
        SELECT id, 'application-desk', '0.1.0', 'active' FROM platform.tenants
        ON CONFLICT (tenant_id, module_key) DO NOTHING;
    END IF;
END;
$$;
