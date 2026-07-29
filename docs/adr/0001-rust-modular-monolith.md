# ADR 0001: Rust modular monolith with independent domain modules

- Status: Accepted
- Date: 2026-07-29

Use one Cargo workspace and independently bounded module crates while the platform
is young. Deploy a small number of processes, preserve module boundaries in code
and data, and extract services only when measured scaling, isolation, or team
ownership requires it. Modules can be registered, configured, activated, and
upgraded independently through the platform module registry.