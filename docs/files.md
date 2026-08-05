# File APIs

File upload, download, signed URLs, antivirus scanning, object storage and file metadata HTTP endpoints are **Not Implemented**.

A `crates/files` package and Documents module scaffold exist, but they expose no mounted Axum routes. Clients must not call presumed `/files` or `/documents` endpoints.

When implemented, document MIME allowlists, maximum sizes, checksums, tenant-scoped object keys, authorization, malware scanning, retention and deletion behavior.
