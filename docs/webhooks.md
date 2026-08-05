# Webhooks

Inbound and outbound HTTP webhooks are **Not Implemented**.

CRM writes transactional rows to `crm.outbox_events`, but no webhook subscription model, signature scheme, delivery worker, retry schedule, dead-letter queue or webhook endpoint is mounted.

The current outbox is consumed only by the CRM WebSocket polling loop. External integrations must not assume webhook delivery until a versioned contract and signature verification are implemented.
