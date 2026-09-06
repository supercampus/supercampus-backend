CREATE TABLE IF NOT EXISTS platform.maintenance_windows (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    enabled boolean NOT NULL DEFAULT false,
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    message text NOT NULL DEFAULT 'SuperCampus is temporarily unavailable while scheduled maintenance is completed.',
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT maintenance_window_time_order CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS maintenance_windows_active_idx
    ON platform.maintenance_windows (enabled, starts_at, ends_at);
