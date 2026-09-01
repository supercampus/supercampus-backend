-- Librarian operations: managed capacity, auditable announcements and admin approval.

CREATE TABLE IF NOT EXISTS campus_ops.library_settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    slot_capacity integer NOT NULL DEFAULT 500 CHECK (slot_capacity BETWEEN 1 AND 10000),
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO campus_ops.library_settings (tenant_id,slot_capacity,updated_by)
SELECT id,500,'runtime-migration-0083' FROM platform.tenants
ON CONFLICT (tenant_id) DO NOTHING;

CREATE TABLE IF NOT EXISTS campus_ops.library_announcements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    title text NOT NULL,
    message text NOT NULL,
    book_title text,
    author text,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','approved','rejected')),
    created_by text NOT NULL,
    created_by_name text NOT NULL,
    decision_note text,
    decided_by text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS library_announcements_queue_idx
    ON campus_ops.library_announcements (tenant_id,status,created_at DESC);

INSERT INTO authz.permission_templates
    (permission_key,module_key,feature_key,action,display_name,description,crud_actions,active)
VALUES
    ('library.capacity.manage','library','occupancy','update','Manage library slots','Set the available library booking capacity',ARRAY['update']::text[],true),
    ('library.logs.read','library','visit_history','read','Export library logs','Read and export library request history',ARRAY['read']::text[],true),
    ('library.announcement.create','library','announcements','create','Create library announcements','Submit book and library announcements for approval',ARRAY['create']::text[],true),
    ('library.announcement.approve','library','announcements','approve','Approve library announcements','Approve or reject librarian announcements',ARRAY['update']::text[],true)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,crud_actions=EXCLUDED.crud_actions,
    active=true,updated_at=now();

INSERT INTO authz.permission_definitions
    (tenant_id,permission_key,module_key,feature_key,action,display_name,description,crud_actions,active)
SELECT tenant.id,template.permission_key,template.module_key,template.feature_key,
       template.action,template.display_name,template.description,template.crud_actions,true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key IN
    ('library.capacity.manage','library.logs.read','library.announcement.create','library.announcement.approve')
ON CONFLICT (tenant_id,permission_key) DO UPDATE SET
    module_key=EXCLUDED.module_key,feature_key=EXCLUDED.feature_key,
    action=EXCLUDED.action,display_name=EXCLUDED.display_name,
    description=EXCLUDED.description,crud_actions=EXCLUDED.crud_actions,
    active=true,updated_at=now();

INSERT INTO authz.role_permissions
    (tenant_id,role_id,permission_key,surface,scope,constraints,granted_by,granted_at)
SELECT role.tenant_id,role.id,permission.permission_key,surface.name,'all','{}'::jsonb,
       'runtime-migration-0083',now()
FROM authz.roles role
JOIN (VALUES
    ('librarian','library.capacity.manage'),
    ('librarian','library.logs.read'),
    ('librarian','library.announcement.create'),
    ('tenant_admin','library.announcement.approve'),
    ('admin','library.announcement.approve'),
    ('administrator','library.announcement.approve'),
    ('super_admin','library.announcement.approve')
) permission(role_key,permission_key) ON permission.role_key=role.role_key
CROSS JOIN (VALUES ('app'::text),('website'::text)) surface(name)
ON CONFLICT (tenant_id,role_id,surface,permission_key) DO UPDATE SET
    scope=EXCLUDED.scope,constraints=EXCLUDED.constraints,
    granted_by=EXCLUDED.granted_by,granted_at=EXCLUDED.granted_at;

