# API versioning and deprecation

## Current versioning

Platform and CRM business endpoints use the path prefix `/api/v1`. Authentication, state, health and readiness endpoints are currently unversioned under `/api` or the root.

The API package version is `0.1.0`. There is no automated compatibility checker or version-negotiation header.

## Policy

- Add breaking business-API changes under a new major path such as `/api/v2`.
- Prefer additive fields and endpoints within `v1`.
- Do not repurpose an existing field or silently change its type.
- Keep compatibility aliases only while they are documented and tested.
- Authentication changes that alter request/response shape should receive a versioned migration plan even though current paths are unversioned.

## Deprecation

A production deprecation mechanism is **Not Implemented**. Adopt:

- `Deprecation: true` and `Sunset` response headers.
- A replacement `Link` header.
- At least 90 days notice for external consumers.
- Changelog entries and migration examples.
- Usage monitoring before removal.

Use [api-changelog-template.md](api-changelog-template.md) for each release.
