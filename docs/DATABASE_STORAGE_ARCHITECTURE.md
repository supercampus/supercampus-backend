# SuperCampus database-per-institution architecture

## Current runtime architecture

SuperCampus uses **one institution = one PostgreSQL database**. A single PostgreSQL service, including one Dokploy PostgreSQL service, can host the control database and many independently named institution databases. A separate PostgreSQL container is not required for every institution.

    SuperCampus Web
           |
    Rust Platform API
           |
    SuperCampusControl (control plane)
           |
    platform.tenant_databases
       |         |         |
    Tenant A  Tenant B  Tenant C

CONTROL_DATABASE_URL points to the control database. The API reuses its server, port, user, password, and TLS settings when opening a pool for an institution; the registry stores only a validated logical database name. Database names permit letters, digits, and underscores only.

## Control plane

The control database is the source of truth for cross-institution identity and routing:

- platform.tenants: institution catalog and status.
- platform.tenant_databases: institution-to-database registry.
- identity.users: globally unique login identities.
- identity.tenant_memberships: the institution selected server-side after email/password validation.
- identity.auth_sessions: refresh-session state and revocation.
- authz.permission_definitions: tenant-admin-managed permission catalog.
- authz.roles, authz.role_permissions, authz.user_roles: dynamic RBAC assignments.

Login never accepts a tenant or campus identifier. The control plane validates the email/password, selects the primary active membership, creates the session, and signs the institution slug into the JWT tid claim.

## Institution databases

Every institution database is migrated independently and holds that institution's operational data:

- CRM leads, forms, form submissions, communications, pipelines, campaigns, counselor capacity, audit records, and outbox events.
- Dynamic module records.
- Runtime configuration documents.
- Per-user UI state.
- Future academics, fees, attendance, exams, hostel, transport, admissions, and other module schemas.

Rows still carry tenant_id and CRM retains transaction-local tenant context and PostgreSQL RLS. This is defense in depth inside a physically isolated database, not the primary isolation boundary.

Campuses are business records inside the institution database. They are not tenants, database selectors, or login inputs.

## Request routing

1. The client sends email/password to POST /api/auth/login.
2. The control plane resolves the user's primary active institution membership.
3. The API issues a JWT containing sub, tid, sid, and roles.
4. Authentication middleware verifies the JWT and control-plane session.
5. Authorization is loaded from the control database.
6. TenantDatabaseManager resolves tid through platform.tenant_databases.
7. Tenant-owned platform and CRM operations execute on that institution's cached PostgreSQL pool.
8. Attempts to use a tenant header different from the signed tid are rejected.

Pools are opened lazily, migrated before use, cached by institution slug, and checked by /ready.

## Environment

    CONTROL_DATABASE_URL=postgresql://user:password@postgres:5432/SuperCampusControl

DATABASE_URL is no longer used by the running API. It is accepted only by the one-time split-control-plane migration command as the existing shared/source database.

## Migration and provisioning commands

Migrate the control database and all registered institution databases:

    cargo run -p supercampus-migration-runner -- migrate

Split an existing single-institution database into a control plane plus registered institution database:

    $env:DATABASE_URL = "postgresql://.../DevSuperCampus"
    $env:CONTROL_DATABASE_URL = "postgresql://.../SuperCampusControl"
    $env:PRIMARY_TENANT_SLUG = "tenant-local"
    cargo run -p supercampus-migration-runner -- split-control-plane

The split is forward-only and idempotent. It copies only the chosen institution's identities, memberships, sessions, roles, and permissions to the control plane. Existing operational data remains in its current institution database.

Provision a new database for an institution already created in the control plane:

    cargo run -p supercampus-migration-runner -- provision tenant-university-b tenant_university_b

Provisioning creates the logical database if needed, runs every runtime migration, installs the exact institution row, registers the database, and verifies the connection.

## Dokploy

Use one PostgreSQL service and create multiple logical databases within it:

    PostgreSQL service
    ├── SuperCampusControl
    ├── DevSuperCampus
    ├── tenant_university_b
    └── tenant_college_c

Deploy the migration runner before the API. The API must receive CONTROL_DATABASE_URL; each institution database is discovered from the registry. Database backup, restore, retention, and point-in-time recovery can therefore be handled per institution.

## Isolation invariants

- Tenant database selection comes only from the verified JWT membership, never a public request field.
- A registered institution maps to exactly one active logical database.
- The registry never stores database passwords.
- Every tenant-owned query still carries tenant context.
- Unknown or inactive registry entries fail closed.
- Database names are validated before being used as PostgreSQL identifiers.
- Identity and RBAC administration stays in the control plane.
- Operational data never falls back to another tenant's database.