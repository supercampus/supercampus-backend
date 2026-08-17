-- Canonical subject delivery and staff authority assignments.
-- Existing departments, academic years, terms, sections and employees remain
-- the source of truth; this migration only connects them.

CREATE TABLE IF NOT EXISTS core.subjects (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    department_id uuid NOT NULL,
    code text NOT NULL,
    name text NOT NULL,
    credits numeric(5,2),
    active boolean NOT NULL DEFAULT true,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.subject_offerings (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    subject_id uuid NOT NULL,
    academic_year_id uuid NOT NULL,
    term_id uuid,
    section_id uuid NOT NULL,
    active boolean NOT NULL DEFAULT true,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, subject_id)
        REFERENCES core.subjects (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, term_id)
        REFERENCES core.terms (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, section_id)
        REFERENCES core.sections (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS subject_offerings_without_term_uidx
    ON core.subject_offerings (tenant_id, subject_id, academic_year_id, section_id)
    WHERE term_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS subject_offerings_with_term_uidx
    ON core.subject_offerings
        (tenant_id, subject_id, academic_year_id, term_id, section_id)
    WHERE term_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS core.department_authorities (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    department_id uuid NOT NULL,
    user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    authority_role text NOT NULL DEFAULT 'hod' CHECK (authority_role = 'hod'),
    starts_on date,
    ends_on date,
    active boolean NOT NULL DEFAULT true,
    assigned_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    CHECK (ends_on IS NULL OR starts_on IS NULL OR ends_on >= starts_on),
    UNIQUE (tenant_id, department_id, user_id, authority_role),
    UNIQUE (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS department_authorities_user_idx
    ON core.department_authorities (tenant_id, user_id, active);

CREATE TABLE IF NOT EXISTS core.teaching_assignments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    subject_offering_id uuid NOT NULL,
    faculty_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    assignment_type text NOT NULL DEFAULT 'primary'
        CHECK (assignment_type IN ('primary', 'co_faculty', 'substitute')),
    active boolean NOT NULL DEFAULT true,
    assigned_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, subject_offering_id)
        REFERENCES core.subject_offerings (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, subject_offering_id, faculty_user_id, assignment_type),
    UNIQUE (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS teaching_assignments_faculty_idx
    ON core.teaching_assignments (tenant_id, faculty_user_id, active);

CREATE TABLE IF NOT EXISTS core.academic_assignment_audit (
    id bigserial PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    actor_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    resource_type text NOT NULL
        CHECK (resource_type IN ('department_authority', 'teaching_assignment')),
    resource_id uuid NOT NULL,
    action text NOT NULL CHECK (action IN ('assigned', 'updated', 'removed')),
    before_state jsonb,
    after_state jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS academic_assignment_audit_lookup_idx
    ON core.academic_assignment_audit (tenant_id, resource_type, resource_id, created_at DESC);

DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'core.subjects',
        'core.subject_offerings',
        'core.department_authorities',
        'core.teaching_assignments',
        'core.academic_assignment_audit'
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

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
VALUES
    ('academics.assignments.read', 'academics', 'assignments', 'read', ARRAY['read']::text[], 'View academic assignments', 'View departments, subjects, sections and staff assignments within the granted scope', true)
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
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template
  ON template.permission_key = 'academics.assignments.read'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();
