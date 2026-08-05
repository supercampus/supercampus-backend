# Data model and tenant isolation

SuperCampus uses a control-plane database plus one operational PostgreSQL database per institution.

| Database | Primary schemas and responsibility |
|---|---|
| Control plane | platform institution registry, identity users/memberships/sessions, authz dynamic roles and permissions |
| Institution database | crm, configuration, platform.dynamic_records, identity.ui_states, and future module schemas |

The signed JWT tid identifies an institution. platform.tenant_databases maps that slug to one logical database name. The API never chooses a database from an untrusted login field or request header.

Tenant-owned tables retain tenant_id foreign keys and CRM retains RLS for defense in depth. Physical database separation is the primary boundary; tenant predicates and RLS prevent accidental cross-context access within a database.

See DATABASE_STORAGE_ARCHITECTURE.md for routing, migration, provisioning, Dokploy deployment, and operational invariants.