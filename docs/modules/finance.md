# Finance / Fees module

**Status: Not Implemented as a dedicated HTTP API.**

The fees package is scaffolded; invoices, collections, refunds, ledgers, and payment webhooks are not implemented.

The generic route `/api/v1/{module_key}/records` may store arbitrary tenant JSON for registered modules, but it does not provide this module's validation, authorization, workflows, domain tables, events, reports, or integrations.
