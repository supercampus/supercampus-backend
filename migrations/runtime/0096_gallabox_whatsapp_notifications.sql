-- Durable, consent-aware WhatsApp delivery for campus notification events.
ALTER TABLE campus_ops.notification_preferences
    ADD COLUMN IF NOT EXISTS whatsapp_enabled boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS whatsapp_opted_in_at timestamptz;

ALTER TABLE campus_ops.notifications
    ADD COLUMN IF NOT EXISTS whatsapp_attempt_count integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS whatsapp_last_error text,
    ADD COLUMN IF NOT EXISTS whatsapp_sent_at timestamptz;

CREATE TABLE IF NOT EXISTS campus_ops.notification_whatsapp_deliveries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    notification_id uuid NOT NULL REFERENCES campus_ops.notifications(id) ON DELETE CASCADE,
    recipient_user_id text NOT NULL,
    recipient_name text NOT NULL,
    phone text NOT NULL,
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'processing', 'retrying', 'sent', 'failed')),
    attempt_count integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    locked_at timestamptz,
    provider_message_id text,
    last_error text,
    sent_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, notification_id, recipient_user_id)
);

CREATE INDEX IF NOT EXISTS campus_ops_notification_whatsapp_queue_idx
    ON campus_ops.notification_whatsapp_deliveries
       (tenant_id, next_attempt_at, created_at)
    WHERE status IN ('queued', 'retrying');

COMMENT ON COLUMN campus_ops.notification_preferences.whatsapp_enabled IS
    'True only after the account holder has opted in to WhatsApp notifications.';
