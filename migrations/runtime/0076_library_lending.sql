CREATE SCHEMA IF NOT EXISTS library;

CREATE TABLE IF NOT EXISTS library.lending_settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id) ON DELETE CASCADE,
    loan_days integer NOT NULL DEFAULT 30 CHECK (loan_days BETWEEN 1 AND 365),
    fine_per_day numeric(10,2) NOT NULL DEFAULT 5 CHECK (fine_per_day >= 0),
    updated_by uuid REFERENCES identity.users(id),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS library.books (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    isbn text,
    accession_number text,
    catalog_key text GENERATED ALWAYS AS (
        COALESCE(NULLIF(lower(btrim(isbn)), ''), 'accession:' || lower(btrim(accession_number)))
    ) STORED,
    title text NOT NULL,
    author text NOT NULL DEFAULT '',
    category text NOT NULL DEFAULT '',
    shelf_code text NOT NULL DEFAULT '',
    total_copies integer NOT NULL DEFAULT 1 CHECK (total_copies > 0),
    available_copies integer NOT NULL DEFAULT 1 CHECK (available_copies >= 0),
    active boolean NOT NULL DEFAULT true,
    created_by uuid REFERENCES identity.users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (available_copies <= total_copies)
);

CREATE UNIQUE INDEX IF NOT EXISTS library_books_isbn_unique
    ON library.books (tenant_id, lower(isbn)) WHERE isbn IS NOT NULL AND btrim(isbn) <> '';
CREATE UNIQUE INDEX IF NOT EXISTS library_books_accession_unique
    ON library.books (tenant_id, lower(accession_number))
    WHERE accession_number IS NOT NULL AND btrim(accession_number) <> '';
CREATE UNIQUE INDEX IF NOT EXISTS library_books_catalog_key_unique
    ON library.books (tenant_id, catalog_key);
CREATE INDEX IF NOT EXISTS library_books_catalog_idx
    ON library.books (tenant_id, active, lower(title));

CREATE TABLE IF NOT EXISTS library.book_loans (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    book_id uuid NOT NULL REFERENCES library.books(id),
    student_user_id uuid NOT NULL REFERENCES identity.users(id),
    roll_number text NOT NULL,
    student_name text NOT NULL,
    status text NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested','approved','rejected','returned','cancelled')),
    requested_at timestamptz NOT NULL DEFAULT now(),
    approved_at timestamptz,
    due_at timestamptz,
    returned_at timestamptz,
    decided_by uuid REFERENCES identity.users(id),
    decision_note text,
    renewal_count integer NOT NULL DEFAULT 0 CHECK (renewal_count >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS library_one_open_loan_per_book_student
    ON library.book_loans (tenant_id, book_id, student_user_id)
    WHERE status IN ('requested','approved');
CREATE INDEX IF NOT EXISTS library_book_loans_student_idx
    ON library.book_loans (tenant_id, student_user_id, requested_at DESC);
CREATE INDEX IF NOT EXISTS library_book_loans_pending_idx
    ON library.book_loans (tenant_id, status, requested_at);

CREATE TABLE IF NOT EXISTS library.book_favourites (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_user_id uuid NOT NULL REFERENCES identity.users(id) ON DELETE CASCADE,
    book_id uuid NOT NULL REFERENCES library.books(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, student_user_id, book_id)
);

INSERT INTO library.lending_settings (tenant_id)
SELECT id FROM platform.tenants
ON CONFLICT (tenant_id) DO NOTHING;

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description, crud_actions, active)
VALUES
    ('library.catalog.read', 'library', 'catalog', 'read', 'Browse library books', 'Browse the institution library catalogue', ARRAY['read']::text[], true),
    ('library.loan.create', 'library', 'loan', 'create', 'Request a library book', 'Request and renew own library loans', ARRAY['create']::text[], true),
    ('library.loan.read', 'library', 'loan', 'read', 'View library loans', 'View own library loans', ARRAY['read']::text[], true),
    ('library.favourite.update', 'library', 'favourite', 'update', 'Manage favourite books', 'Add or remove own favourite books', ARRAY['update']::text[], true),
    ('library.catalog.manage', 'library', 'catalog', 'manage', 'Manage library catalogue', 'Add and bulk import books', ARRAY['create','read','update']::text[], true),
    ('library.loan.approve', 'library', 'loan', 'approve', 'Approve library loans', 'Approve, reject and return book loans', ARRAY['read','update']::text[], true),
    ('library.settings.update', 'library', 'settings', 'update', 'Configure lending', 'Set loan duration and daily overdue fine', ARRAY['read','update']::text[], true)
ON CONFLICT (permission_key) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name, description, crud_actions, active)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.display_name, template.description, template.crud_actions, true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key LIKE 'library.%'
ON CONFLICT (tenant_id, permission_key) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT role.tenant_id, role.id, grant_row.permission_key, grant_row.scope, '{}'::jsonb,
       'runtime-migration-0076'
FROM authz.roles role
JOIN (VALUES
    ('student', 'library.catalog.read', 'institution'),
    ('student', 'library.loan.create', 'own'),
    ('student', 'library.loan.read', 'own'),
    ('student', 'library.favourite.update', 'own'),
    ('librarian', 'library.catalog.read', 'institution'),
    ('librarian', 'library.catalog.manage', 'institution'),
    ('librarian', 'library.loan.read', 'institution'),
    ('librarian', 'library.loan.approve', 'institution'),
    ('librarian', 'library.settings.update', 'institution')
) AS grant_row(role_key, permission_key, scope)
  ON grant_row.role_key = role.role_key
ON CONFLICT (tenant_id, role_id, permission_key) DO UPDATE SET
    scope = EXCLUDED.scope,
    granted_by = EXCLUDED.granted_by,
    granted_at = now();
