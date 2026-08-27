-- Attendance sessions must retain the complete timetable block. Without the
-- ending slot, a two-period class was reported only against its first period
-- in the student attendance heatmap.

UPDATE campus_ops.attendance_sessions AS session
SET period_label = CASE
        WHEN entry.block_length > 1
          THEN starting_slot.label || '-' || ending_slot.label
        ELSE starting_slot.label
    END,
    updated_at = now()
FROM core.timetable_entries AS entry
JOIN core.timetable_slots AS starting_slot
  ON starting_slot.id = entry.slot_id
JOIN core.timetable_entries AS ending_entry
  ON ending_entry.tenant_id = entry.tenant_id
 AND ending_entry.version_id = entry.version_id
 AND ending_entry.session_block_id = entry.session_block_id
 AND ending_entry.block_sequence = entry.block_length
JOIN core.timetable_slots AS ending_slot
  ON ending_slot.id = ending_entry.slot_id
WHERE session.tenant_id = entry.tenant_id
  AND session.timetable_entry_id = entry.id
  AND entry.block_sequence = 1
  AND session.period_label IS DISTINCT FROM CASE
        WHEN entry.block_length > 1
          THEN starting_slot.label || '-' || ending_slot.label
        ELSE starting_slot.label
      END;
