# Notification worker operations

The `supercampus-notification-worker` delivers outbound CRM email/SMS and
consent-aware campus push/WhatsApp events. Run one or more replicas; row leases
and `SKIP LOCKED` make concurrent replicas safe.

## Required environment

- `CONTROL_DATABASE_URL`
- `APP_ENV=production` or `staging`
- SMTP: `SMTP_HOST`, `SMTP_PORT`, `MAIL_FROM`, and either both
  `SMTP_USER`/`SMTP_PASSWORD` or neither when the relay authenticates by network
- Twilio account: `TWILIO_ACCOUNT_SID` and either
  `TWILIO_API_KEY_SID`/`TWILIO_API_KEY_SECRET` or `TWILIO_AUTH_TOKEN`
- SMS: either `TWILIO_SMS_FROM` or `TWILIO_MESSAGING_SERVICE_SID`
- Gallabox WhatsApp: `WHATSAPP_PROVIDER=gallabox`, `GALLABOX_API_KEY`,
  `GALLABOX_API_SECRET`, `GALLABOX_CHANNEL_ID`, and approved event template
  names. `GALLABOX_ACCOUNT_ID` is retained for account diagnostics/template
  listing but is not sent as a message credential.

Production and staging startup fail when a channel is partially configured.
This prevents queued messages from being falsely recorded as delivered.

## Deployment

1. Deploy all migrations, including the WhatsApp delivery queue.
2. Verify the sender domains/numbers and provider templates.
3. Start the worker separately from the API:
   `supercampus-notification-worker`.
4. Send one controlled message per channel to institution-owned test recipients.
5. Confirm the communication row becomes `sent` and stores the provider ID.
6. Confirm a deliberately invalid test recipient retries and becomes `failed`
   after five attempts without blocking newer jobs.

Do not place credentials in source control or Dokploy build arguments. Supply
them as runtime secrets and rotate anything previously shared in chat or logs.

## Gallabox template contract

Create approved Utility templates for attendance, gatepass, fees, examination,
library, hostel and transport. The worker supplies these named body values:
`RecipientName`, `Title`, `Message`, `EventType`, optional `ActionUrl`, and when
present in fee records `Amount`, `DueDate`, and `Status`. The fee template may
use one dynamic URL button at index 0; set `SUPERCAMPUS_APP_URL` so the worker
can populate the payment CTA. A Gallabox Payment Template can replace that CTA
without changing event routing once its provider payload is approved.

WhatsApp delivery is opt-in. The preferences API accepts an explicit
`whatsappEnabled: true` per category and records the consent timestamp. Older
clients that only update push settings preserve the existing WhatsApp choice.
