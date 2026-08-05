# Request and response lifecycle

```mermaid
sequenceDiagram
    participant Client
    participant MW as JWT/session middleware
    participant Handler
    participant Service
    participant DB as PostgreSQL
    Client->>MW: HTTP request + bearer/cookie
    MW->>MW: Verify algorithm, signature, issuer, audience, expiry
    MW->>DB: Resolve active session and tenant
    MW->>Handler: Canonical tenant/user/role context
    Handler->>Service: Parsed path/query/body DTO
    Service->>DB: Permission audit and tenant transaction
    DB-->>Service: Tenant-scoped result
    Service-->>Handler: Domain result/error
    Handler-->>Client: JSON envelope + HTTP status
```

For CRM writes, history, aggregate changes and outbox events are generally committed together. Form enquiry lead creation plus submission uses compensation rather than one cross-operation transaction. WebSocket requests upgrade after the same middleware and then poll tenant outbox events.
