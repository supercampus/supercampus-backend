# SuperCampus backend contributor guide

This repository is a Rust Cargo workspace. Keep domain behavior inside its module,
shared platform capabilities inside `crates`, and runnable processes inside `apps`.

- Every module must implement the standard manifest, migration, OpenAPI,
  domain/application/infrastructure/API/jobs, and test boundaries.
- Domain code must not depend on Axum, SQLx, messaging clients, or deployment code.
- Every database query and event must carry tenant context.
- Add schema changes as forward-only SQL migrations.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` before submitting changes.
- Never commit `.env`, credentials, tokens, private keys, production tenant data,
  Node packages, or generated `node_modules`.