# SuperCampus push notification morning test

## Before testing

1. Deploy the API image built from this workspace. The image now includes the
   API, migration runner, and notification worker.
2. Run migrations through `0074_push_notification_foundation.sql` for every
   tenant database.
3. Start `supercampus-notification-worker` with:
   - `APP_ENV=production`
   - `FCM_ENABLED=true`
   - `FIREBASE_PROJECT_ID=supercampus-production-5b1d9`
   - `GOOGLE_APPLICATION_CREDENTIALS=/run/secrets/firebase-admin.json`
4. Mount the Firebase service-account JSON read-only at that path. Never copy
   it into the repository or container image.
5. Install the latest APK, log in, and allow Android notifications.

## Five-minute smoke test

1. Log in as a student. Confirm a row for the phone appears in
   `campus_ops.push_devices`.
2. Credit that student's wallet. Expect **Wallet credited**; tapping it opens
   the canteen/wallet area.
3. Place a canteen order. The assigned owner/captains should receive
   **New canteen order**. Change its status; the student should receive the
   update.
4. Submit a gatepass. The next approver should be notified. Approve it;
   security and the student should be notified. Scan it; the student should
   receive the entry/exit confirmation.
5. Publish an attendance roll. The student receives attendance status, then
   the HOD/Principal receives the configured review notification.
6. Publish a timetable or request a faculty substitution. Students/faculty or
   the next decision-maker should receive the matching actionable alert.
7. Create/update a student-targeted fee, result, library, hostel, or transport
   record with the student's user ID. Confirm the notification opens the
   related module.

## Delivery checks

- Inbox notification appears even if the phone is offline.
- Foreground messages display through the high-priority Android channel.
- Tapping a background/terminated notification opens the related module.
- Repeating the same business mutation does not duplicate a deduplicated alert.
- Logging out unregisters the token; logging back in registers the current one.
- Invalid/expired FCM tokens become disabled while other devices continue.
- `campus_ops.notification_push_deliveries.status` reaches `sent` and contains
  the provider message ID.

## Security follow-up

Rotate the service-account key before the production launch because the current
key file was shared during setup. Update only the deployed secret mount; do not
commit the replacement JSON.
