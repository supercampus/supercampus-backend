# Notification worker operations

The `supercampus-notification-worker` delivers outbound CRM email, SMS, and
WhatsApp jobs. Run exactly one or more replicas; row leases and `SKIP LOCKED`
make concurrent replicas safe.

## Required environment

- `CONTROL_DATABASE_URL`
- `APP_ENV=production` or `staging`
- SMTP: `SMTP_HOST`, `SMTP_PORT`, `MAIL_FROM`, and either both
  `SMTP_USER`/`SMTP_PASSWORD` or neither when the relay authenticates by network
- Twilio account: `TWILIO_ACCOUNT_SID` and either
  `TWILIO_API_KEY_SID`/`TWILIO_API_KEY_SECRET` or `TWILIO_AUTH_TOKEN`
- SMS: either `TWILIO_SMS_FROM` or `TWILIO_MESSAGING_SERVICE_SID`
- WhatsApp: `TWILIO_WHATSAPP_FROM` and the approved content/template settings
  already used by the API

Production and staging startup fail when a channel is partially configured.
This prevents queued messages from being falsely recorded as delivered.

## Deployment

1. Deploy migration 69 through the normal migration runner/API startup.
2. Verify the sender domains/numbers and provider templates.
3. Start the worker separately from the API:
   `supercampus-notification-worker`.
4. Send one controlled message per channel to institution-owned test recipients.
5. Confirm the communication row becomes `sent` and stores the provider ID.
6. Confirm a deliberately invalid test recipient retries and becomes `failed`
   after five attempts without blocking newer jobs.

Do not place credentials in source control or Dokploy build arguments. Supply
them as runtime secrets and rotate anything previously shared in chat or logs.
