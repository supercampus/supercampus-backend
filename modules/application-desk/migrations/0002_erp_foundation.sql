-- Canonical ERP foundation shared by every module.
-- Domain modules reference these records by ID and must not duplicate them.

CREATE SCHEMA IF NOT EXISTS core;

CREATE TABLE IF NOT EXISTS core.campuses (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    code text NOT NULL,
    name text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.academic_years (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    code text NOT NULL,
    name text NOT NULL,
    starts_on date NOT NULL,
    ends_on date NOT NULL,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'closed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_on >= starts_on),
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS academic_year_one_active_per_tenant
    ON core.academic_years (tenant_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS core.terms (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    academic_year_id uuid NOT NULL,
    code text NOT NULL,
    name text NOT NULL,
    sequence integer NOT NULL,
    starts_on date,
    ends_on date,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'closed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, academic_year_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.departments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    campus_id uuid,
    code text NOT NULL,
    name text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, campus_id)
        REFERENCES core.campuses (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.programmes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    department_id uuid NOT NULL,
    code text NOT NULL,
    name text NOT NULL,
    duration_terms integer,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.batches (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    programme_id uuid NOT NULL,
    academic_year_id uuid NOT NULL,
    code text NOT NULL,
    name text NOT NULL,
    starts_on date,
    ends_on date,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, programme_id)
        REFERENCES core.programmes (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, programme_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.sections (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    batch_id uuid NOT NULL,
    code text NOT NULL,
    name text NOT NULL,
    capacity integer CHECK (capacity IS NULL OR capacity > 0),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, batch_id)
        REFERENCES core.batches (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, batch_id, code),
    UNIQUE (tenant_id, id)
);

-- Student Master remains the canonical person-in-student-role identity.
-- Academic context is stored separately in academic_enrollments.
CREATE UNIQUE INDEX IF NOT EXISTS students_tenant_id_idx
    ON core.students (tenant_id, id);

CREATE TABLE IF NOT EXISTS core.academic_enrollments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_id uuid NOT NULL,
    academic_year_id uuid NOT NULL,
    term_id uuid,
    campus_id uuid,
    department_id uuid NOT NULL,
    programme_id uuid NOT NULL,
    batch_id uuid NOT NULL,
    section_id uuid,
    status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('provisional', 'active', 'completed', 'withdrawn', 'cancelled')),
    started_at timestamptz NOT NULL DEFAULT now(),
    ended_at timestamptz,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, term_id)
        REFERENCES core.terms (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, campus_id)
        REFERENCES core.campuses (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, programme_id)
        REFERENCES core.programmes (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, batch_id)
        REFERENCES core.batches (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, section_id)
        REFERENCES core.sections (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, student_id, academic_year_id, programme_id),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.guardians (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id uuid REFERENCES identity.users(id) ON DELETE SET NULL,
    full_name text NOT NULL,
    email text,
    phone text,
    profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, user_id)
);

CREATE TABLE IF NOT EXISTS core.student_guardians (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_id uuid NOT NULL,
    guardian_id uuid NOT NULL,
    relationship text NOT NULL,
    is_primary boolean NOT NULL DEFAULT false,
    permissions jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, student_id, guardian_id),
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, guardian_id)
        REFERENCES core.guardians (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS core.employees (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id uuid REFERENCES identity.users(id) ON DELETE SET NULL,
    employee_number text NOT NULL,
    department_id uuid,
    full_name text NOT NULL,
    email text,
    phone text,
    status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('provisional', 'active', 'inactive', 'terminated')),
    profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, employee_number),
    UNIQUE (tenant_id, user_id),
    UNIQUE (tenant_id, id)
);

-- Tenant isolation is enforced in the database as well as application services.
DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'core.campuses', 'core.academic_years', 'core.terms',
        'core.departments', 'core.programmes', 'core.batches', 'core.sections',
        'core.academic_enrollments', 'core.guardians',
        'core.student_guardians', 'core.employees'
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
-- Cases created by the retired Application Submitted trigger stay in history but
-- are closed so they neither appear as active onboarding nor block the later
-- Offer Accepted handoff.
WITH retired_candidates AS MATERIALIZED (
    SELECT tenant_id, id, stage, status
    FROM application_desk.cases
    WHERE document->>'crmLeadId' IS NOT NULL
      AND COALESCE(document->'attributes'->>'handoffReason', '') <> 'offer_accepted'
      AND status NOT IN ('COMPLETED', 'REJECTED', 'CANCELLED', 'WITHDRAWN', 'EXPIRED')
), retired_handoffs AS (
    UPDATE application_desk.cases
    SET status = 'CANCELLED',
        document = jsonb_set(
            jsonb_set(document, '{status}', '"CANCELLED"'::jsonb, true),
            '{rejectionReason}',
            '"Retired pre-offer CRM handoff"'::jsonb,
            true
        ),
        updated_at = now()
    FROM retired_candidates candidate
    WHERE application_desk.cases.tenant_id = candidate.tenant_id
      AND application_desk.cases.id = candidate.id
    RETURNING application_desk.cases.tenant_id, application_desk.cases.id
)
INSERT INTO application_desk.audit_log
    (tenant_id, case_id, actor, action, from_stage, to_stage,
     from_status, to_status, reason)
SELECT candidate.tenant_id, candidate.id, 'system', 'retire_pre_offer_handoff',
       candidate.stage, candidate.stage, candidate.status, 'CANCELLED',
       'Application submission no longer opens Admission Desk; awaiting Offer Accepted'
FROM retired_candidates candidate
JOIN retired_handoffs retired
  ON retired.tenant_id = candidate.tenant_id AND retired.id = candidate.id;


-- Preserve the stable module key while aligning the user-facing product name.
DO $$
BEGIN
    IF to_regclass('platform.navigation_sections') IS NOT NULL THEN
        UPDATE platform.navigation_sections
        SET label = 'Admission Desk', updated_at = now()
        WHERE section_key = 'application-desk';
    END IF;
END;
$$;
