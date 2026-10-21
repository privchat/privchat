# Call Control Plane

Calls are disabled unless `[call] enabled=true`, a `ws://` or `wss://` LiveKit URL,
and the configured API key/secret environment variables are available. Missing
credentials fail closed: capabilities report disabled. Never put secrets in a
shared config or client bundle.

```toml
[call]
enabled = false
livekit_url = "wss://media.example.invalid"
api_key_env = "PRIVCHAT_LIVEKIT_API_KEY"
api_secret_env = "PRIVCHAT_LIVEKIT_API_SECRET"
token_ttl_seconds = 60
ringing_timeout_seconds = 45
heartbeat_lease_seconds = 60
audio_participant_limit = 16
video_participant_limit = 9
```

Mutations use the typed `rpc::call` contracts. A command is durably keyed by
authenticated `(user_id, device_id, command_id)` and fingerprints both operation
and typed request. A different payload with the same key is rejected. The command,
state change and side-effect intents commit together. Do not reuse a command ID
for a new heartbeat; replay returns the previous result without extending its lease.

The initiator owns its starting device. A participant's first successful join
claims its device permanently for that call. Other devices receive PermissionDenied
with `Call accepted on another device`. Only the owning device can heartbeat,
leave, or end its joined call. A fresh join command on the same device can refresh
credentials while authorized; an expired cached token is not silently reissued by
replaying its old command. Ended/removed/blocked participants cannot replay a join
command to obtain credentials. Participant device identifiers are not broadcast.

`call/state` uses a direct per-user authenticated-session PublishRequest with
JSON `{ "call": CallSnapshot }`; it does not depend on a chat subscription. Events
are hints and may repeat. Compare monotonically increasing `call.version` and use
`call/get` / `call/active` after reconnect or a gap. Active returns that user's
invitations and joined calls, not every call in a group. Call lifecycle clocks are
server milliseconds; `expires_at_ms` is the ringing deadline, not the media JWT TTL.

A background worker expires ringing/participant leases and processes durable
state, history and media-removal intents. The terminal summary goes through
MessageService and the existing timeline commit/delivery outbox, with permanent
`call-summary:<call_id>` deduplication. Its System content is JSON containing
`event=call.ended`, `call_id`, `media_kind`, `duration_ms` and `end_reason` because
System metadata is not represented in the current canonical wire metadata union.
System summaries follow existing System unread-count behavior.

## Media Security Boundary

Room names are unique per call. Join JWTs grant only that room, microphone for
audio, microphone/camera for video, and no data or screen-share publishing. The
server alone signs them. DeleteRoom requires a server-only roomCreate credential;
RemoveParticipant uses roomAdmin scoped to one room. Neither credential is sent
to clients. Operations use bounded HTTP timeouts and durable retries.

Self-hosted LiveKit does NOT revoke old tokens when a participant is removed or a
room is deleted. A valid cached token can recreate/rejoin that room until the SFU
rejects it; token expiry applies to initial connection and LiveKit can refresh
connected clients' tokens. Thus a 60-second issued TTL plus DeleteRoom is NOT an
instantaneous revocation guarantee or a hard bound on an already connected
session. The IM state is terminal and refuses new credentials, but strict media
revocation requires a separately enforced SFU admission/revocation mechanism.
No claim of immediate self-hosted revocation is made.

Official references verified during implementation:
- https://docs.livekit.io/frontends/reference/tokens-grants/
- https://docs.livekit.io/reference/other/roomservice-api/

This implementation does not add PushKit/CallKit or Android full-screen push.
Offline invitations reconcile through active/get; background wake-up is separate.

## Reproducible Acceptance

`tests/call_state_db_test.rs` creates and destroys its own databases via the normal
migration runner. Supply `PRIVCHAT_TEST_DATABASE_URL` pointing to a local test
PostgreSQL instance with database-creation rights; no production URL is permitted.

- `cargo check --tests`
- `cargo test --test call_state_db_test`
- `cargo test --lib rpc::call::tests::injected_visibility_read_failure_retries_instead_of_completing_effect -- --exact`

The explicit `livekit_real_join_and_delete_room` test additionally requires
`PRIVCHAT_CALL_LIVEKIT_FIXTURE` (a private test YAML file) and a running loopback
LiveKit on port 56780. Run it with `--ignored --exact`. It authenticates an actual
WebSocket upgrade using an issued token, then performs idempotent room deletion;
it is not an audio/video track transport test.

`tests/call_im_runtime_test.rs` requires an explicitly created isolated IM fixture
via `PRIVCHAT_CALL_IM_RUNTIME`, and runs only with `--ignored`. It tests ordinary
password login and authentication, direct Publish downlink without subscription,
active-list reconciliation, device contention, real SFU credential handshake,
terminal summary delivery through the existing message outbox, and replay of the
post-message-commit crash window. The fixture continuously acknowledges normal
message pushes, decodes the received System message's compatible JSON envelope,
verifies the stored canonical event, and matches the received message ID and pts
against an authenticated history RPC with no local cache.
Replay must restore the call's history marker and finish the effect without a
second message. The outbox assertion is only a fresh-write check: completed
outbox rows are retained for 24 hours, not permanently.
It also locks the call row to inject an actual
PostgreSQL visibility-read timeout: the effect must remain pending and recover
when the lock is released. Only PermissionDenied is a permanent recipient skip.
