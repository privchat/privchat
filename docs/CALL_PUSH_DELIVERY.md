# Offline incoming call delivery

Owned source changes: `src/rpc/call.rs`, `src/service/call_config.rs`,
`src/service/call_service.rs`, new `src/service/call_service/push.rs`, new
`src/service/call_service/push/provider.rs`, new `migrations/010_call_push.sql`,
new `tests/call_push_delivery_test.rs`, and this document. The only paired protocol
change is the additive DTOs/constants in `../privchat-protocol/src/rpc/call.rs`.
No other domain, dependency pin, applied migration or native app was edited.

This is a call-owned extension, not a chat message/Publish fallback. Existing one-way
`call/state`, `call/get`, `call/active`, peer WebRTC and LiveKit group behavior remain
authoritative. Push messages are expiring hints, not permission to join a call.

## Frozen mobile contract

Authenticated RPC `call/push/register` accepts exactly:

```json
{"platform":"apns_voip","token":"<PushKit token hex>"}
```

Android uses `platform: "fcm"`. Response: `{"registered":true}`. User and device
are exclusively from the authenticated RPC context. The device must exist, belong
to that user, be active and have the corresponding iOS/Android platform. Unknown
fields, including user/device IDs, topic, environment and provider endpoints, are
rejected. The route retrieves the authenticated session version from the existing
session manager; the transaction requires it to match the active device row before
binding a token. An old connection cannot silently re-arm a revoked registration. An invalid or
unconfigured provider returns `ServiceUnavailable`, never successful registration.

Authenticated `call/push/unregister` accepts exactly `{}` and returns
`{"registered":false}`. Cleanup is idempotent and cancels pending deliveries for
this account/device only. Clients unregister before logout and re-register after
token rotation, account switch or renewed device sessions. VoIP tokens are stored
separately and never replace ordinary APNs chat tokens.

Every payload value below is a string. APNs wraps this under `data`, alongside
`aps.content-available: 1`; FCM uses `message.data` without a `notification` block:

```json
{
  "schema_version":"1",
  "type":"call.incoming",
  "call_uuid":"<UUIDv5 URL namespace of privchat:call:<call_id>>",
  "call_id":"123",
  "channel_id":"456",
  "initiator_id":"1",
  "recipient_user_id":"2",
  "media_kind":"audio",
  "transport":"webrtc",
  "version":"1",
  "expires_at_ms":"1792800000000"
}
```

`media_kind` is `audio|video`; `transport` is `webrtc|livekit`.
`call.cancel` has the same fields and a short control-delivery deadline. IDs and
versions remain decimal strings, including beyond JavaScript's safe integer range.
`call_uuid` is stable across attempts and devices. No SDP, ICE candidates, media
tokens, TURN credentials, caller-controlled URLs or account credentials are included.
`initiator_id`/`recipient_user_id` are the agreed caller/callee field names; no aliases.

iOS must promptly report a genuine PushKit incoming call to CallKit before completing
the PushKit callback. Reconcile through authenticated call APIs and end stale calls;
do not wait for a network request before reporting. Android must handle its processing
window with an appropriate system call notification/lifecycle. Both clients must bind
the recipient to the current account, deduplicate, enforce expiry and reconcile
version/state; delivery can race cancellation or be duplicated after a worker crash.
No new client-side authority should compete with the shared system-call bridge.

## Operator configuration and migration

Apply the normal migration command with the rebuilt server. New migration
`010_call_push.sql` creates dedicated registrations and durable device outbox tables;
no prior migrations are modified and there is no new runtime startup DDL.
The registration foreign key uses the existing `(user_id, device_id)` device key.

```toml
[call.push]
enabled = true
max_attempts = 4 # 1..8; default enabled=false
```

Also enable the existing `[push]` infrastructure and the corresponding provider:

- APNs: existing `push.apns.enabled`, `bundle_id`, `team_id`, `key_id`,
  `private_key_path` and `use_sandbox`. `bundle_id` is the ordinary application
  bundle, not the `.voip` topic. The backend derives the VoIP topic; no client can
  select sandbox, topic or host. APNs uses ES256 provider tokens cached for 50 minutes.
- FCM: existing `push.fcm.enabled`, `service_account_path`, optional `project_id`
  and `https_proxy`. RSA service-account OAuth is refreshed before expiry. The call
  adapter deliberately does not accept the ordinary provider's pasted access-token
  development fallback. OAuth/provider hosts are fixed, not taken from a request or
  the credential file's `token_uri`.
- Keep credential files private. Registration DTOs and notification structs do not
  derive `Debug`; provider bodies, bearer tokens and device tokens are not logged.

If provider construction fails, call state/RPC continues but registration fails
closed. Enabling this feature does not enable TURN or change any media default.
Existing `call.peer.allow_turn=false` remains unchanged.

## Delivery semantics

- Invitations enqueue in the same transaction as call state. Late token registration
  can enqueue an already-ringing eligible invitation. Command replay does not duplicate
  per-call/device entries. Account switches transfer a physical token away from the
  former registration.
- Before dispatch, recheck call state, deadline, membership, contact blocking, account
  status, device ownership, active session and registered session version. Expired,
  answered, revoked or unknown devices must not receive a new incoming request.
- Initial genuine iOS invitations alone use `apns-push-type: voip`, priority 10,
  expiration 0 and derived `<bundle>.voip`. APNs has no storage deadline beyond this
  immediate attempt. FCM is data-only HIGH priority with TTL bounded by ring expiry.
- Answering cancels pending incoming entries on all accepting user's devices, except
  that no cancellation is delivered to the winning device while its call is active.
  Cancellation is NEVER VoIP: ordinary armed APNs token, background/priority 5/
  expiration 0, or FCM data-only HIGH/TTL 0. Missing ordinary APNs control token is
  recorded `unavailable`, not as sent. Existing `call/state` and client timers/reconcile
  are essential because background control delivery is not guaranteed.
- Worker outcomes persist as pending/sent/cancelled/expired/failed/unavailable. Transient
  network/401/403/429/5xx errors retry with bounded 2/4/8-second backoff and an attempt
  cap. Known invalid incoming tokens are removed only if token and session still match,
  so rotation cannot be erased. Other rejection is not reported as success.
- The worker locks the call before the outbox and bounds the entire HTTP/OAuth operation
  to at most 3 seconds (or the remaining deadline), to serialize an answer against an
  in-flight invitation. An already accepted/in-flight push cannot be recalled. Provider
  acceptance is not proof of delivery, CallKit display, or real-device audio.
- At-least-once: a crash after provider acceptance but before transaction commit can
  redeliver the stable delivery ID. Clients must not create duplicate system calls.

## Focused acceptance

`tests/call_push_delivery_test.rs` creates a fresh uniquely named PostgreSQL database,
applies all production migrations, and sends real HTTP requests to a loopback Axum
provider. The fixture bearer tokens are explicitly nonproduction and the production
factory cannot select the fixture. No test claims real Apple/Google or device delivery.

```sh
cd /Users/zoujiaqing/projects/privchat/privchat-server
env -u PRIVCHAT_ALLOW_SKIPPING_DB_TESTS \
  DATABASE_URL='postgresql://<test-role>:<test-password>@127.0.0.1:<port>/<migrated-compile-db>' \
  PRIVCHAT_TEST_DATABASE_URL='postgresql://<test-role>:<test-password>@127.0.0.1:<port>/postgres' \
  cargo test --locked --test call_push_delivery_test --test call_state_db_test \
    -- --test-threads=1 --nocapture
```

The separate compile DB must already contain the current schema for SQLx query macros;
it is not a production database. Each integration test applies the production migration
runner to its own disposable database. The role needs permission to create/drop these
isolated test databases. Missing DB
configuration fails this suite instead of silently passing. Coordinate the shared
Cargo queue before executing. Do not alter the parent's running IM fixture.

Coverage: dual-device APNs/FCM payloads and idempotence; winner exclusion; background
non-VoIP cancellation; worker restart and durable bounded retries; unknown/wrong-owner/
wrong-platform rejection; strict DTOs; missing provider; session-version revocation;
token normalization/invalidation; expiry; unregister; answer-before-delivery suppression.
Execution status must be reported separately from these implemented test cases.

### Current execution evidence

On 2026-10-24, all ten production SQL migration files applied successfully to a new
uniquely named disposable PostgreSQL database at loopback port 55476. The test
database was removed afterward. Sanitized evidence:
`/tmp/privchat-call-push-sql.B4FI4p/migrations.log` and
`/tmp/privchat-call-push-sql.B4FI4p/constraints.txt` (confirms the composite device FK).
This was a direct SQL migration-file check, not the Rust migration-runner test.

Actual coordinated execution on 2026-10-24:

- `call_push_delivery_test`: **4 passed, 0 failed, 0 ignored**, 9.11 seconds. Actual
  PostgreSQL and loopback HTTP requests, not a mock provider trait or skipped DB suite.
- `call_state_db_test`: **12 passed, 0 failed, 1 ignored**, 30.33 seconds. The ignored
  test is `livekit_real_join_and_delete_room`, which requires a separately owned SFU.
- Initial test compilation found two incorrect argument lists after adding authenticated
  session-version checking. Only the test calls were corrected; the initial failure
  log is retained. The final test compilation took 19.98 seconds using the rebuilt lib.
- `cargo test` also rebuilt the normal `privchat` server executable (not a Rust test
  harness). A subsequent explicit `cargo build --bin privchat` began a redundant full
  library build because of dependency-feature unification differences. That extra
  build and its own children were deliberately interrupted to conserve disk; it is
  **not** reported as a successful explicit build. No external build was interrupted.
- Frozen regular server: `/tmp/privchat-call-push-run.twJZhC/artifacts/privchat-call-cohort`.
  SHA-256: `59c00ae1d531f5bcea8f69920bc19aa955ce602f4c84f70aa6da6912586bd67e`.
- The frozen executable itself applied **all ten embedded migrations** on another new
  disposable DB, including the corrected composite FK in 010. A second run was
  idempotent. The migration ledger's 010 SHA matched the source:
  `2a6262483adb4d23f02e029e607eeca133b534667def65a29a83139e4eb25350`.
- All owned disposable compile, integration and binary-migration databases were removed.
  The source cohort was verified unchanged. No running IM fixture was restarted or
  migrated. No credentialed APNs/FCM request or physical-device delivery is claimed.

Evidence under `/tmp/privchat-call-push-run.twJZhC/`:
`tests.log`, `tests-initial-compile-error.log`, `binary-migrations.log`,
`binary-migrations-idempotent.log`, `binary-migration-proof.txt`,
`source-cohort.sha256`, `source-cohort-post-test-verified.log`, `test-source.sha256`,
`environment.txt`, `build.log` and `build-decision.txt`.
The private `run.sh` and `freeze-and-check.sh` reproduce the disposable host setup;
they are test-host-specific and are not production deployment scripts.

### Logout boundary

The existing authenticated logout path attempts to bump the device session version;
forced kick increments it and sets the device inactive. The push worker suppresses
registrations with an old version or inactive device. However, the existing logout
handler logs and swallows a failed DB version bump before returning success.
Local-only/offline logout also cannot guarantee server revocation. Call clients should
unregister while still authenticated when possible, clear local call state on close,
and enforce recipient/account binding on receipt. Do not race an old-account async
unregister against a newly switched client. No shared auth/logout behavior was changed
in this call-owned patch.

## Primary platform references

- [Apple: Responding to VoIP notifications](https://developer.apple.com/documentation/pushkit/responding-to-voip-notifications-from-pushkit)
- [Firebase: Android message priority](https://firebase.google.com/docs/cloud-messaging/android-message-priority)
- [Firebase: Message lifespan and TTL](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-lifespan)
- [Android: CallStyle notifications](https://developer.android.com/develop/ui/compose/notifications/call-style)

Credentialed Apple/Google acceptance, killed-app/locked physical-device notification,
CallKit/Android Telecom actions, account-switch device behavior and actual media paths
remain separate end-to-end acceptance requirements. Local HTTP success does not prove
any of them.
