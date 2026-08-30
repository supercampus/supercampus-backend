-- Tenant-scoped notification inbox, preferences, and push-device registry.
ALTER TABLE campus_ops.notifications
    ADD COLUMN IF NOT EXISTS event_type text NOT NULL DEFAULT 'general.notice',
    ADD COLUMN IF NOT EXISTS priority text NOT NULL DEFAULT 'normal',
    ADD COLUMN IF NOT EXISTS requires_action boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS deep_link text,
    ADD COLUMN IF NOT EXISTS deduplication_key text,
    ADD COLUMN IF NOT EXISTS expires_at timestamptz,
    ADD COLUMN IF NOT EXISTS push_attempt_count integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS push_last_error text,
    ADD COLUMN IF NOT EXISTS push_sent_at timestamptz;

UPDATE campus_ops.notifications
SET event_type = category || '.notification'
WHERE event_type = 'general.notice' AND category <> 'general';

UPDATE campus_ops.notifications
SET deep_link = CASE category
    WHEN 'attendance' THEN '/academics/attendance'
    WHEN 'canteen' THEN '/shops/orders'
    WHEN 'gatepass' THEN '/gatepass'
    WHEN 'fees' THEN '/tuition-fee'
    WHEN 'timetable' THEN '/timetable'
    WHEN 'examination' THEN '/examinations'
    ELSE deep_link
END
WHERE deep_link IS NULL;

CREATE INDEX IF NOT EXISTS campus_ops_notifications_unread_idx
    ON campus_ops.notifications (tenant_id, recipient_user_id, created_at DESC)
    WHERE read_at IS NULL;

CREATE INDEX IF NOT EXISTS campus_ops_notifications_push_queue_idx
    ON campus_ops.notifications (tenant_id, created_at)
    WHERE push_status IN ('queued', 'retrying');

CREATE UNIQUE INDEX IF NOT EXISTS campus_ops_notifications_deduplication_idx
    ON campus_ops.notifications (tenant_id, deduplication_key)
    WHERE deduplication_key IS NOT NULL;

-- A role-targeted notification is shared by many users, so each viewer needs
-- an independent read receipt instead of changing the shared notification.
CREATE TABLE IF NOT EXISTS campus_ops.notification_receipts (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    notification_id uuid NOT NULL REFERENCES campus_ops.notifications(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    read_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, notification_id, user_id)
);

CREATE TABLE IF NOT EXISTS campus_ops.push_devices (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    provider text NOT NULL DEFAULT 'fcm' CHECK (provider IN ('fcm', 'apns', 'web_push')),
    platform text NOT NULL CHECK (platform IN ('android', 'ios', 'web')),
    token text NOT NULL,
    device_name text,
    locale text,
    enabled boolean NOT NULL DEFAULT true,
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, token)
);

CREATE INDEX IF NOT EXISTS campus_ops_push_devices_user_idx
    ON campus_ops.push_devices (tenant_id, user_id)
    WHERE enabled;

-- Delivery is tracked per device so a partial provider failure can be retried
-- without sending duplicates to devices that already accepted the message.
CREATE TABLE IF NOT EXISTS campus_ops.notification_push_deliveries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    notification_id uuid NOT NULL REFERENCES campus_ops.notifications(id) ON DELETE CASCADE,
    device_id uuid NOT NULL REFERENCES campus_ops.push_devices(id) ON DELETE CASCADE,
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'processing', 'retrying', 'sent', 'invalid', 'failed')),
    attempt_count integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    locked_at timestamptz,
    provider_message_id text,
    last_error text,
    sent_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, notification_id, device_id)
);

CREATE INDEX IF NOT EXISTS campus_ops_notification_push_delivery_queue_idx
    ON campus_ops.notification_push_deliveries (tenant_id, next_attempt_at, created_at)
    WHERE status IN ('queued', 'retrying');

CREATE TABLE IF NOT EXISTS campus_ops.notification_preferences (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    category text NOT NULL,
    push_enabled boolean NOT NULL DEFAULT true,
    digest_enabled boolean NOT NULL DEFAULT false,
    quiet_hours_start time,
    quiet_hours_end time,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id, category),
    CHECK ((quiet_hours_start IS NULL) = (quiet_hours_end IS NULL))
);

