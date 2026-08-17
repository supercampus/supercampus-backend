-- Workload and consecutive-block requirements used by the timetable allocator.
-- Values belong to a tenant's subject offering; no institution-specific course
-- or room inventory is hard-coded here.

ALTER TABLE core.rooms DROP CONSTRAINT IF EXISTS rooms_room_type_check;
ALTER TABLE core.rooms ADD CONSTRAINT rooms_room_type_check CHECK (room_type IN (
    'classroom', 'tutorial_room', 'laboratory', 'computer_lab',
    'chemistry_lab', 'physics_lab', 'workshop', 'library', 'staff_room',
    'seminar_hall', 'auditorium', 'sports', 'other'
));

UPDATE core.timetable_configurations
SET rules = jsonb_set(rules, '{requiredSectionPeriodsPerWeek}', '35'::jsonb, true),
    updated_at = now()
WHERE rules ->> 'preset' = 'anna-university-2025'
  AND NOT (rules ? 'requiredSectionPeriodsPerWeek');

CREATE TABLE IF NOT EXISTS core.subject_offering_workload_requirements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    subject_offering_id uuid NOT NULL,
    delivery_type text NOT NULL DEFAULT 'class'
        CHECK (delivery_type IN ('class', 'laboratory', 'tutorial', 'project', 'activity')),
    periods_per_week smallint NOT NULL CHECK (periods_per_week BETWEEN 1 AND 35),
    block_size smallint NOT NULL DEFAULT 1 CHECK (block_size BETWEEN 1 AND 7),
    max_blocks_per_day smallint NOT NULL DEFAULT 1 CHECK (max_blocks_per_day BETWEEN 1 AND 7),
    required_room_types text[] NOT NULL DEFAULT ARRAY[]::text[],
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by uuid NOT NULL REFERENCES identity.users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, subject_offering_id)
        REFERENCES core.subject_offerings (tenant_id, id) ON DELETE CASCADE,
    UNIQUE (tenant_id, subject_offering_id, delivery_type),
    UNIQUE (tenant_id, id)
);

ALTER TABLE core.timetable_entries
    ADD COLUMN IF NOT EXISTS session_block_id uuid,
    ADD COLUMN IF NOT EXISTS block_sequence smallint NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS block_length smallint NOT NULL DEFAULT 1;

UPDATE core.timetable_entries
SET session_block_id = id
WHERE session_block_id IS NULL;

ALTER TABLE core.timetable_entries
    ALTER COLUMN session_block_id SET NOT NULL;

ALTER TABLE core.timetable_entries DROP CONSTRAINT IF EXISTS timetable_entries_block_sequence_check;
ALTER TABLE core.timetable_entries ADD CONSTRAINT timetable_entries_block_sequence_check
    CHECK (block_sequence BETWEEN 1 AND block_length AND block_length BETWEEN 1 AND 7);

CREATE UNIQUE INDEX IF NOT EXISTS timetable_entry_block_sequence_uidx
    ON core.timetable_entries (tenant_id, version_id, session_block_id, block_sequence);

CREATE INDEX IF NOT EXISTS timetable_workload_offering_idx
    ON core.subject_offering_workload_requirements (tenant_id, subject_offering_id);

DO $$
DECLARE
    target text;
BEGIN
    FOREACH target IN ARRAY ARRAY[
        'core.subject_offering_workload_requirements'
    ] LOOP
        IF to_regclass(target) IS NOT NULL THEN
            EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', target);
            EXECUTE format('DROP POLICY IF EXISTS tenant_isolation ON %s', target);
            EXECUTE format(
                'CREATE POLICY tenant_isolation ON %s USING (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid) WITH CHECK (tenant_id = nullif(current_setting(''app.tenant_id'', true), '''')::uuid)',
                target
            );
        END IF;
    END LOOP;
END $$;
