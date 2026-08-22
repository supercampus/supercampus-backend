CREATE SCHEMA IF NOT EXISTS campus_ops;

CREATE TABLE IF NOT EXISTS campus_ops.events (
    sequence bigserial PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    module_key text NOT NULL,
    aggregate_type text NOT NULL,
    aggregate_id text NOT NULL,
    event_type text NOT NULL,
    actor_user_id text,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS campus_ops_events_tenant_sequence_idx
    ON campus_ops.events (tenant_id, sequence);

CREATE TABLE IF NOT EXISTS campus_ops.notifications (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    recipient_user_id text,
    recipient_role text,
    category text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    data jsonb NOT NULL DEFAULT '{}'::jsonb,
    push_status text NOT NULL DEFAULT 'queued',
    read_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS campus_ops_notifications_recipient_idx
    ON campus_ops.notifications (tenant_id, recipient_user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS campus_ops.canteen_menu_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    name text NOT NULL,
    description text NOT NULL DEFAULT '',
    category text NOT NULL CHECK (category IN ('meals', 'snacks', 'drinks')),
    price numeric(12,2) NOT NULL CHECK (price >= 0),
    prep_minutes integer NOT NULL DEFAULT 10 CHECK (prep_minutes > 0),
    is_vegetarian boolean NOT NULL DEFAULT true,
    is_popular boolean NOT NULL DEFAULT false,
    is_available boolean NOT NULL DEFAULT true,
    image_url text,
    created_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS campus_ops.canteen_wallets (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    balance numeric(14,2) NOT NULL DEFAULT 0 CHECK (balance >= 0),
    version bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE TABLE IF NOT EXISTS campus_ops.canteen_wallet_transactions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    amount numeric(14,2) NOT NULL CHECK (amount <> 0),
    transaction_type text NOT NULL CHECK (transaction_type IN ('manual_top_up', 'online_top_up', 'order_debit', 'refund')),
    description text NOT NULL,
    reference_id text,
    idempotency_key text,
    actor_user_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS campus_ops.canteen_orders (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    order_number bigint GENERATED ALWAYS AS IDENTITY,
    customer_user_id text NOT NULL,
    customer_name text NOT NULL,
    lines jsonb NOT NULL,
    total numeric(14,2) NOT NULL CHECK (total >= 0),
    fulfilment_mode text NOT NULL CHECK (fulfilment_mode IN ('dine_in', 'pickup')),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'preparing', 'ready', 'completed', 'rejected', 'cancelled')),
    token_number integer,
    qr_token_hash text NOT NULL UNIQUE,
    handled_by text,
    rejection_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS campus_ops_canteen_orders_queue_idx
    ON campus_ops.canteen_orders (tenant_id, status, created_at);

CREATE TABLE IF NOT EXISTS campus_ops.canteen_staff_state (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    mode text NOT NULL DEFAULT 'eat' CHECK (mode IN ('eat', 'work')),
    shop_open boolean,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE TABLE IF NOT EXISTS campus_ops.gatepass_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    requester_user_id text NOT NULL,
    requester_name text NOT NULL,
    pass_type text NOT NULL CHECK (pass_type IN ('outpass', 'leave_pass')),
    residency text NOT NULL CHECK (residency IN ('day_scholar', 'hosteller')),
    destination text NOT NULL,
    reason text NOT NULL,
    guardian_phone text,
    departure_at timestamptz NOT NULL,
    return_at timestamptz NOT NULL,
    state text NOT NULL,
    workflow jsonb NOT NULL,
    qr_token_hash text,
    decided_by text,
    decision_note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (return_at > departure_at)
);
CREATE INDEX IF NOT EXISTS campus_ops_gatepass_queue_idx
    ON campus_ops.gatepass_requests (tenant_id, state, created_at);

CREATE TABLE IF NOT EXISTS campus_ops.gatepass_approvals (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    request_id uuid NOT NULL REFERENCES campus_ops.gatepass_requests(id) ON DELETE CASCADE,
    step_key text NOT NULL,
    decision text NOT NULL CHECK (decision IN ('approved', 'rejected')),
    actor_user_id text NOT NULL,
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, request_id, step_key)
);

CREATE TABLE IF NOT EXISTS campus_ops.gate_movements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    request_id uuid REFERENCES campus_ops.gatepass_requests(id) ON DELETE SET NULL,
    direction text NOT NULL CHECK (direction IN ('entry', 'exit')),
    checkpoint text NOT NULL,
    scanned_by text NOT NULL,
    method text NOT NULL DEFAULT 'qr',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS campus_ops.daily_access_passes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    valid_on date NOT NULL,
    qr_token_hash text NOT NULL UNIQUE,
    activated_latitude double precision NOT NULL,
    activated_longitude double precision NOT NULL,
    activated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, user_id, valid_on)
);

CREATE TABLE IF NOT EXISTS campus_ops.parent_student_links (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    parent_user_id text NOT NULL,
    student_user_id text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, parent_user_id, student_user_id)
);

CREATE TABLE IF NOT EXISTS campus_ops.attendance_sessions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    subject_offering_id uuid,
    section_id uuid,
    subject_name text NOT NULL,
    faculty_user_id text NOT NULL,
    held_on date NOT NULL,
    period_label text NOT NULL,
    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published_to_hod', 'returned', 'submitted_to_principal')),
    hod_note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS campus_ops.attendance_entries (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES campus_ops.attendance_sessions(id) ON DELETE CASCADE,
    student_user_id text NOT NULL,
    student_name text NOT NULL,
    status text NOT NULL CHECK (status IN ('present', 'absent', 'od', 'leave')),
    marked_by text NOT NULL,
    marked_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, session_id, student_user_id)
);

CREATE TABLE IF NOT EXISTS campus_ops.attendance_reports (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    title text NOT NULL,
    period_start date NOT NULL,
    period_end date NOT NULL,
    department_id uuid,
    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted_to_principal', 'acknowledged')),
    generated_by text NOT NULL,
    summary jsonb NOT NULL DEFAULT '{}'::jsonb,
    submitted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT permission_key, module_key, feature_key, action, crud_actions, display_name, description, true
FROM (VALUES
    ('canteen.menu.read', 'canteen', 'menu', 'read', ARRAY['read']::text[], 'View canteen menu', 'View available canteen menu items'),
    ('canteen.menu.create', 'canteen', 'menu', 'create', ARRAY['create']::text[], 'Add menu items', 'Add canteen menu items'),
    ('canteen.menu.update', 'canteen', 'menu', 'update', ARRAY['update']::text[], 'Manage menu items', 'Edit availability and menu details'),
    ('canteen.menu.delete', 'canteen', 'menu', 'delete', ARRAY['delete']::text[], 'Delete menu items', 'Delete canteen menu items'),
    ('canteen.order.create', 'canteen', 'order', 'create', ARRAY['create']::text[], 'Place canteen orders', 'Create canteen orders from the menu'),
    ('canteen.order.read', 'canteen', 'order', 'read', ARRAY['read']::text[], 'View canteen orders', 'View own or assigned canteen orders'),
    ('canteen.orders.manage', 'canteen', 'orders', 'update', ARRAY['update']::text[], 'Manage order queue', 'Accept, reject, prepare and complete orders'),
    ('canteen.analytics.read', 'canteen', 'analytics', 'read', ARRAY['read']::text[], 'View canteen analytics', 'View canteen sales and operations analytics'),
    ('canteen.wallet.top_up', 'canteen', 'wallet', 'update', ARRAY['update']::text[], 'Top up wallets', 'Credit a campus user wallet'),
    ('gatepass.outpass.create', 'gatepass', 'outpass', 'create', ARRAY['create']::text[], 'Create outpass', 'Create a hosteller outpass request'),
    ('gatepass.outpass.read', 'gatepass', 'outpass', 'read', ARRAY['read']::text[], 'View outpasses', 'View own or assigned outpass requests'),
    ('gatepass.outpass.approve', 'gatepass', 'outpass', 'approve', ARRAY['update']::text[], 'Approve outpasses', 'Approve or reject an outpass workflow step'),
    ('gatepass.leave.create', 'gatepass', 'leave', 'create', ARRAY['create']::text[], 'Create leave pass', 'Create a leave pass request'),
    ('gatepass.leave.read', 'gatepass', 'leave', 'read', ARRAY['read']::text[], 'View leave passes', 'View leave pass requests'),
    ('gatepass.leave.approve', 'gatepass', 'leave', 'approve', ARRAY['update']::text[], 'Approve leave passes', 'Approve or reject leave pass steps'),
    ('gatepass.scan.create', 'gatepass', 'scan', 'create', ARRAY['create']::text[], 'Scan gate QR', 'Record gate entry and exit scans'),
    ('gatepass.scan.read', 'gatepass', 'scan', 'read', ARRAY['read']::text[], 'View gate movements', 'View gate entry and exit logs'),
    ('gatepass.access.read', 'gatepass', 'access', 'read', ARRAY['read']::text[], 'View daily gate access', 'View the current daily gate-in credential'),
    ('attendance.roster.read', 'attendance', 'roster', 'read', ARRAY['read']::text[], 'View attendance roster', 'View students assigned to an attendance roster'),
    ('attendance.roster.update', 'attendance', 'roster', 'update', ARRAY['update']::text[], 'Mark attendance roster', 'Mark present, absent, OD and leave attendance'),
    ('attendance.session.create', 'attendance', 'session', 'create', ARRAY['create']::text[], 'Create attendance session', 'Create class attendance sessions'),
    ('attendance.session.publish', 'attendance', 'session', 'publish', ARRAY['update']::text[], 'Publish attendance', 'Publish marked attendance to the HOD'),
    ('attendance.records.read', 'attendance', 'records', 'read', ARRAY['read']::text[], 'View attendance records', 'View attendance totals and history'),
    ('attendance.reports.create', 'attendance', 'reports', 'create', ARRAY['create']::text[], 'Create attendance report', 'Create HOD attendance reports'),
    ('attendance.reports.publish', 'attendance', 'reports', 'publish', ARRAY['update']::text[], 'Submit attendance report', 'Submit HOD reports to the principal'),
    ('attendance.parent.read', 'attendance', 'parent', 'read', ARRAY['read']::text[], 'View child attendance', 'View linked student attendance and alerts')
) AS permission(permission_key, module_key, feature_key, action, crud_actions, display_name, description)
ON CONFLICT (permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, crud_actions, display_name, description, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.crud_actions, template.display_name, template.description, true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key IN (
    'canteen.menu.read','canteen.menu.create','canteen.menu.update','canteen.menu.delete',
    'canteen.order.create','canteen.order.read','canteen.orders.manage','canteen.analytics.read',
    'canteen.wallet.top_up','gatepass.outpass.create','gatepass.outpass.read','gatepass.outpass.approve',
    'gatepass.leave.create','gatepass.leave.read','gatepass.leave.approve','gatepass.scan.create',
    'gatepass.scan.read','gatepass.access.read','attendance.roster.read','attendance.roster.update',
    'attendance.session.create','attendance.session.publish','attendance.records.read',
    'attendance.reports.create','attendance.reports.publish','attendance.parent.read'
)
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    module_key = EXCLUDED.module_key, feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action, crud_actions = EXCLUDED.crud_actions,
    display_name = EXCLUDED.display_name, description = EXCLUDED.description,
    active = true, updated_at = now();
