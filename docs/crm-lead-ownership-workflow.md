# CRM lead ownership workflow

Decision date: 2026-08-09

This workflow supersedes the separate unassigned-pool/self-claim user experience.

- New leads are visible in the normal Enquiry column without an owner.
- The first successful stage movement atomically assigns the lead to that actor and moves it.
- The owner can continue moving the card normally.
- A different actor creates a 24-hour, one-use movement request instead of moving the card.
- Only the current owner can approve or reject that request. Approval moves the card but does not transfer ownership.
- A request becomes stale when ownership, stage, or substate changes before approval.
- `crm.leads.stage.override` and `crm.leads.stage.backward` are explicit dynamic permissions; application code does not infer them from role names.

The legacy unassigned-list and explicit-claim endpoints remain temporarily available for API compatibility, but the web workspace no longer exposes them.
