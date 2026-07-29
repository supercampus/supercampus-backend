# Module development

Every domain under `modules` follows one platform contract: a versioned manifest,
isolated migrations, OpenAPI definitions, domain/application boundaries,
infrastructure adapters, HTTP registration, jobs, and tests. Shared concerns
belong in `crates`; module-specific behavior stays inside that module.

A new module requires a unique permission namespace, compatibility checks,
tenant-safe migrations, security review, independent activation controls, and
contract tests. A navigation item or configurable workflow alone is not enough
to justify a new module.