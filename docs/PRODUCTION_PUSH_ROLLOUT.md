# Production push rollout and rollback

## Release artifacts

- Backend Docker image: repository-root `Dockerfile`. It contains
  `supercampus-platform-api`, `supercampus-migration-runner`, and
  `supercampus-notification-worker`.
- Runtime migration: `0074_push_notification_foundation.sql`.
- Android application ID: `ai.supercampus.mobile`.
- Firebase project: `supercampus-production-5b1d9`.

## Dokploy rollout order

1. Back up the control and active tenant databases.
2. Build the backend image from the reviewed branch/commit.
3. Run `supercampus-migration-runner migrate` once. It migrates the control
   database and every active registered tenant database through version 74.
4. Deploy the API using the existing HTTPS health check at `/health`.
5. Create a second Dokploy service from the same image with entrypoint
   `supercampus-notification-worker`.
6. Give the worker the same `CONTROL_DATABASE_URL` as the API and set:

   ```text
   APP_ENV=production
   FCM_ENABLED=true
   FIREBASE_PROJECT_ID=supercampus-production-5b1d9
   GOOGLE_APPLICATION_CREDENTIALS=/run/secrets/firebase-admin.json
   EMAIL_ENABLED=false
   SMS_ENABLED=false
   WHATSAPP_ENABLED=false
   ```

7. Mount the Firebase JSON read-only at
   `/run/secrets/firebase-admin.json`. Do not paste its JSON into an environment
   variable, Dockerfile, build argument, or source repository.
8. Start one worker replica. Confirm its startup log reports `push=fcm` and no
   configuration error. Increase replicas only after the smoke test; row leases
   make multiple replicas safe.
9. Install the current V22 APK, log in, and grant Android notification access.
10. Follow `PUSH_NOTIFICATION_MORNING_TEST.md` and retain screenshots/provider
    IDs for wallet, canteen, gatepass/security, attendance and timetable tests.

## Database verification

Run against the relevant tenant database without copying tokens into output:

```sql
SELECT enabled, platform, provider, count(*)
FROM campus_ops.push_devices
GROUP BY enabled, platform, provider;

SELECT status, count(*)
FROM campus_ops.notification_push_deliveries
GROUP BY status;

SELECT category, push_status, count(*)
FROM campus_ops.notifications
WHERE created_at > now() - interval '1 hour'
GROUP BY category, push_status
ORDER BY category, push_status;
```

Healthy delivery has an enabled Android/FCM device, a per-device delivery in
`sent`, and a provider message ID. A token returning `UNREGISTERED` should be
disabled automatically.

## Rollback

1. Stop the notification-worker service to halt new provider sends.
2. Redeploy the previous API image if an API regression is found.
3. Do **not** reverse or delete migration 74. Its new tables/columns are
   additive and safe for the previous API to ignore.
4. Keep queued notification rows. Restarting the corrected worker resumes them
   using the existing leases and deduplication keys.
5. If a bad event is generating excessive messages, disable only FCM with
   `FCM_ENABLED=false`, deploy the corrected event producer, then re-enable it.

## Credentials and Android signing

- Rotate the Firebase service-account key shared during development before the
  public launch. Replace only the Dokploy secret mount and verify OAuth before
  deleting the old key.
- The current V22 APK is signed with the Android debug certificate and is for
  direct testing only. Before Play Store or managed production distribution,
  create an institution-owned upload keystore, store it in two protected
  backups, copy `android/key.properties.example` to `android/key.properties`,
  and rebuild. The same key must be retained for all future application
  updates.
