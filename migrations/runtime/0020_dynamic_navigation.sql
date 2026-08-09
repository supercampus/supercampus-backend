-- Server-driven navigation.
--
-- Which parts of the workspace a user can see is decided by the tenant administrator
-- through role grants, not by a hardcoded list in the frontend. Each section declares
-- the permissions that reveal it; the API returns only the sections a caller's
-- effective grants satisfy.
CREATE TABLE IF NOT EXISTS platform.navigation_sections (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    section_key text NOT NULL,
    kind text NOT NULL DEFAULT 'workspace',
    label text NOT NULL,
    route text,
    icon text,
    sort_order integer NOT NULL DEFAULT 0,
    -- ANY-of semantics: holding any one of these reveals the section.
    required_permissions text[] NOT NULL DEFAULT '{}'::text[],
    -- When set, holding any permission prefixed "<module_key>." also reveals it.
    module_key text,
    -- Always visible to an authenticated user regardless of grants.
    always_visible boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, section_key),
    CHECK (kind IN ('workspace', 'settings'))
);

CREATE INDEX IF NOT EXISTS navigation_sections_tenant_kind_idx
    ON platform.navigation_sections (tenant_id, kind, sort_order)
    WHERE active;

-- Seed every existing institution with the current workspace layout so behaviour is
-- unchanged at deploy time; administrators can then diverge per tenant.
INSERT INTO platform.navigation_sections
    (tenant_id, section_key, kind, label, route, icon, sort_order,
     required_permissions, module_key, always_visible)
SELECT tenant.id, section.section_key, section.kind, section.label, section.route,
       section.icon, section.sort_order, section.required_permissions,
       section.module_key, section.always_visible
FROM platform.tenants tenant
CROSS JOIN (VALUES
    ('dashboard', 'workspace', 'Dashboard', '/dashboard/admissions', 'LayoutDashboard', 10,
        ARRAY['crm.dashboard.read']::text[], NULL::text, false),
    ('crm', 'workspace', 'CRM', '/dashboard/admissions', 'Target', 20,
        ARRAY['crm.dashboard.read']::text[], NULL::text, false),
    ('pipeline', 'workspace', 'Pipeline', '/dashboard/admissions', 'Kanban', 30,
        ARRAY['crm.leads.read']::text[], NULL::text, false),
    ('admissions', 'workspace', 'Admissions', '/dashboard/admissions', 'ClipboardList', 40,
        ARRAY['crm.erp.handoff']::text[], 'admissions'::text, false),
    ('students', 'workspace', 'Students', '/dashboard/admissions', 'Users', 50,
        ARRAY[]::text[], 'students'::text, false),
    ('academics', 'workspace', 'Academics', '/dashboard/admissions', 'ListChecks', 60,
        ARRAY[]::text[], 'academics'::text, false),
    ('fees', 'workspace', 'Fees & Finance', '/dashboard/admissions', 'Database', 70,
        ARRAY[]::text[], 'fees'::text, false),
    ('erp', 'workspace', 'ERP Services', '/dashboard/admissions', 'Layers', 80,
        ARRAY[]::text[], 'erp'::text, false),
    ('reports', 'workspace', 'Reports & BI', '/dashboard/admissions', 'BarChart3', 90,
        ARRAY['crm.reports.read']::text[], NULL::text, false),
    ('users', 'workspace', 'Users & Roles', '/dashboard/admissions', 'UserCog', 100,
        ARRAY['authorization.users.read', 'authorization.roles.read',
              'authorization.permissions.read']::text[], NULL::text, false),
    ('settings', 'workspace', 'Settings', '/dashboard/admissions', 'Settings', 110,
        ARRAY[]::text[], NULL::text, false),
    ('account', 'settings', 'Account', NULL, 'UserCog', 10,
        ARRAY[]::text[], NULL::text, true),
    ('access', 'settings', 'Access Control', NULL, 'ShieldCheck', 20,
        ARRAY['authorization.permissions.read', 'authorization.roles.read',
              'authorization.users.read']::text[], NULL::text, false),
    ('forms', 'settings', 'Form Builders', NULL, 'ClipboardList', 30,
        ARRAY['crm.forms.read']::text[], NULL::text, false),
    ('workflows', 'settings', 'Workflow Studio', NULL, 'Workflow', 40,
        ARRAY['crm.configuration.read']::text[], NULL::text, false),
    ('theme', 'settings', 'Theme', NULL, 'Palette', 50,
        ARRAY['platform.configuration.update']::text[], NULL::text, false)
) AS section(section_key, kind, label, route, icon, sort_order,
             required_permissions, module_key, always_visible)
ON CONFLICT (tenant_id, section_key) DO NOTHING;
