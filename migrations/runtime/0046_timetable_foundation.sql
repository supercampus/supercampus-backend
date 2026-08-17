-- Versioned, tenant-configurable academic timetable foundation.
-- Limits are defaults for an editable tenant preset, not hard-coded regulatory law.

CREATE TABLE IF NOT EXISTS core.timetable_configurations (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    academic_year_id uuid NOT NULL,
    term_id uuid,
    name text NOT NULL,
    timezone text NOT NULL DEFAULT 'Asia/Kolkata',
    working_days smallint[] NOT NULL DEFAULT ARRAY[1,2,3,4,5]::smallint[],
    max_faculty_periods_per_day smallint NOT NULL DEFAULT 6,
    max_consecutive_faculty_periods smallint NOT NULL DEFAULT 3,
    rules jsonb NOT NULL DEFAULT '{"preset":"anna-university-2025","enforceRoomCapacity":true,"allowCrossSectionElectives":true}'::jsonb,
    active boolean NOT NULL DEFAULT true,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, term_id)
        REFERENCES core.terms (tenant_id, id) ON DELETE RESTRICT,
    CHECK (cardinality(working_days) > 0),
    CHECK (max_faculty_periods_per_day BETWEEN 1 AND 24),
    CHECK (max_consecutive_faculty_periods BETWEEN 1 AND 24),
    UNIQUE (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS timetable_configuration_term_uidx
    ON core.timetable_configurations (tenant_id, academic_year_id, term_id)
    WHERE active AND term_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS core.timetable_slots (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    configuration_id uuid NOT NULL,
    day_of_week smallint NOT NULL CHECK (day_of_week BETWEEN 1 AND 7),
    sequence smallint NOT NULL CHECK (sequence > 0),
    label text NOT NULL,
    slot_type text NOT NULL DEFAULT 'instructional'
        CHECK (slot_type IN ('instructional', 'break', 'lunch')),
    starts_at time NOT NULL,
    ends_at time NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, configuration_id)
        REFERENCES core.timetable_configurations (tenant_id, id) ON DELETE CASCADE,
    CHECK (ends_at > starts_at),
    UNIQUE (tenant_id, configuration_id, day_of_week, sequence),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.rooms (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    campus_id uuid,
    department_id uuid,
    code text NOT NULL,
    name text NOT NULL,
    room_type text NOT NULL DEFAULT 'classroom'
        CHECK (room_type IN ('classroom', 'laboratory', 'workshop', 'seminar_hall', 'auditorium', 'sports', 'other')),
    capacity integer NOT NULL CHECK (capacity > 0),
    features jsonb NOT NULL DEFAULT '[]'::jsonb,
    active boolean NOT NULL DEFAULT true,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, campus_id)
        REFERENCES core.campuses (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, department_id)
        REFERENCES core.departments (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.elective_groups (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    academic_year_id uuid NOT NULL,
    term_id uuid,
    code text NOT NULL,
    name text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, academic_year_id)
        REFERENCES core.academic_years (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, term_id)
        REFERENCES core.terms (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, academic_year_id, code),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.elective_group_sections (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    elective_group_id uuid NOT NULL,
    section_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, elective_group_id, section_id),
    FOREIGN KEY (tenant_id, elective_group_id)
        REFERENCES core.elective_groups (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, section_id)
        REFERENCES core.sections (tenant_id, id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS core.elective_group_students (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    elective_group_id uuid NOT NULL,
    student_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, elective_group_id, student_id),
    FOREIGN KEY (tenant_id, elective_group_id)
        REFERENCES core.elective_groups (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, student_id)
        REFERENCES core.students (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS core.timetable_versions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    configuration_id uuid NOT NULL,
    version_number integer NOT NULL CHECK (version_number > 0),
    label text NOT NULL,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'superseded', 'archived')),
    rules_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    published_by uuid REFERENCES identity.users(id) ON DELETE RESTRICT,
    published_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, configuration_id)
        REFERENCES core.timetable_configurations (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, configuration_id, version_number),
    UNIQUE (tenant_id, id)
);

CREATE UNIQUE INDEX IF NOT EXISTS timetable_one_published_version_uidx
    ON core.timetable_versions (tenant_id, configuration_id)
    WHERE status = 'published';

CREATE TABLE IF NOT EXISTS core.timetable_entries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    version_id uuid NOT NULL,
    slot_id uuid NOT NULL,
    subject_offering_id uuid NOT NULL,
    teaching_assignment_id uuid NOT NULL,
    room_id uuid NOT NULL,
    elective_group_id uuid,
    delivery_type text NOT NULL DEFAULT 'class'
        CHECK (delivery_type IN ('class', 'laboratory', 'tutorial', 'project', 'activity')),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, version_id)
        REFERENCES core.timetable_versions (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, slot_id)
        REFERENCES core.timetable_slots (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, subject_offering_id)
        REFERENCES core.subject_offerings (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, teaching_assignment_id)
        REFERENCES core.teaching_assignments (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, room_id)
        REFERENCES core.rooms (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, elective_group_id)
        REFERENCES core.elective_groups (tenant_id, id) ON DELETE RESTRICT,
    UNIQUE (tenant_id, version_id, subject_offering_id, slot_id),
    UNIQUE (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS timetable_entries_schedule_idx
    ON core.timetable_entries (tenant_id, version_id, slot_id);

CREATE TABLE IF NOT EXISTS core.faculty_substitution_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    timetable_entry_id uuid NOT NULL,
    service_date date NOT NULL,
    original_faculty_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    substitute_faculty_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    reason text NOT NULL,
    status text NOT NULL DEFAULT 'awaiting_acknowledgements'
        CHECK (status IN ('awaiting_acknowledgements', 'awaiting_principal', 'approved', 'rejected', 'cancelled')),
    requested_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    decided_by uuid REFERENCES identity.users(id) ON DELETE RESTRICT,
    decision_note text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, timetable_entry_id)
        REFERENCES core.timetable_entries (tenant_id, id) ON DELETE RESTRICT,
    CHECK (original_faculty_user_id <> substitute_faculty_user_id),
    UNIQUE (tenant_id, timetable_entry_id, service_date, substitute_faculty_user_id),
    UNIQUE (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS core.faculty_substitution_acknowledgements (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    request_id uuid NOT NULL,
    faculty_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    party text NOT NULL CHECK (party IN ('original', 'substitute')),
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    acknowledged_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, request_id, party),
    FOREIGN KEY (tenant_id, request_id)
        REFERENCES core.faculty_substitution_requests (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS core.timetable_events (
    revision bigserial PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    aggregate_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    event_type text NOT NULL,
    actor_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS timetable_events_sync_idx
    ON core.timetable_events (tenant_id, revision);

CREATE OR REPLACE FUNCTION core.reject_timetable_event_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'core.timetable_events is append-only';
END;
$$;

DROP TRIGGER IF EXISTS timetable_events_append_only ON core.timetable_events;
CREATE TRIGGER timetable_events_append_only
    BEFORE UPDATE OR DELETE ON core.timetable_events
    FOR EACH ROW EXECUTE FUNCTION core.reject_timetable_event_mutation();

DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'core.timetable_configurations', 'core.timetable_slots', 'core.rooms',
        'core.elective_groups', 'core.elective_group_sections',
        'core.elective_group_students', 'core.timetable_versions',
        'core.timetable_entries', 'core.faculty_substitution_requests',
        'core.faculty_substitution_acknowledgements', 'core.timetable_events'
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
    ('academics.timetable.read', 'academics', 'timetable', 'read', ARRAY['read']::text[], 'View timetable', 'View the effective timetable within the granted scope', true),
    ('academics.timetable.manage', 'academics', 'timetable', 'update', ARRAY['create','read','update','delete']::text[], 'Manage timetable', 'Configure periods, rooms, electives, draft schedules and publication', true),
    ('academics.timetable.substitution.request', 'academics', 'timetable_substitution', 'create', ARRAY['create']::text[], 'Request Faculty substitution', 'Request a dated Faculty replacement for a scheduled class', true),
    ('academics.timetable.substitution.acknowledge', 'academics', 'timetable_substitution', 'update', ARRAY['update']::text[], 'Acknowledge Faculty substitution', 'Record the original or substitute Faculty acknowledgement', true),
    ('academics.timetable.substitution.approve', 'academics', 'timetable_substitution', 'approve', ARRAY['update']::text[], 'Approve Faculty substitution', 'Principal approval for a fully acknowledged substitution', true)
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
JOIN authz.permission_templates template ON template.permission_key IN (
    'academics.timetable.read', 'academics.timetable.manage',
    'academics.timetable.substitution.request',
    'academics.timetable.substitution.acknowledge',
    'academics.timetable.substitution.approve'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    active = true,
    updated_at = now();
