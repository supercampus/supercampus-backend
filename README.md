# SuperCampus backend

Rust backend workspace for the configuration-driven SuperCampus platform. Shared
crates provide identity, tenancy, authorization, metadata, workflows, rules,
events, audit, storage, observability, and module lifecycle services. Independent
domain crates provide CRM, Admissions, Academics, Attendance, Documents,
Examinations, Fees, Gate Pass, Hostel, Library, Placement, and Transport.

## Prerequisites

- Rust 1.97.1 or newer, selected from `rust-toolchain.toml`
- PostgreSQL 16+

## Local development

```bash
cp .env.example .env
cargo run -p supercampus-platform-api
```

The first Cargo build generates `Cargo.lock`; commit it so production dependency
resolution remains reproducible. The API listens on `127.0.0.1:4000` by default.
Verify with `GET /health` and `GET /api/v1/modules`. Endpoint details and local login credentials are documented in [`docs/api.md`](docs/api.md).

## Quality checks

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

This is a Rust-only repository. Do not add `package.json`, `package-lock.json`,
`node_modules`, Prisma, or Node runtime code to the backend.