-- Durable delivery state for CRM email, SMS, and WhatsApp communications.
ALTER TABLE crm.communications
    ADD COLUMN IF NOT EXISTS attempt_count integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS next_attempt_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS locked_at timestamptz,
    ADD COLUMN IF NOT EXISTS last_error text,
    ADD COLUMN IF NOT EXISTS provider_message_id text,
    ADD COLUMN IF NOT EXISTS sent_at timestamptz,
    ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS crm_communications_delivery_queue_idx
    ON crm.communications (next_attempt_at, created_at)
    WHERE direction = 'outbound'
      AND channel IN ('email', 'sms', 'whatsapp')
      AND status IN ('queued', 'retrying');

-- Calls and notes are recorded interactions, not provider-delivery jobs.
UPDATE crm.communications
SET status = 'completed', updated_at = now()
WHERE direction = 'outbound'
  AND channel IN ('call', 'note')
  AND status = 'queued';
