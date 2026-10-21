# Call Control Plane

## Direct Peer And Group SFU Deployment

New direct calls require `[call.peer] enabled=true` and a peer-capable client
(`supports_peer=true` on invite/join). Missing peer configuration is an explicit
unavailable error, never a silent SFU fallback. Groups always use LiveKit.
Snapshots persist immutable `transport: "webrtc" | "livekit"`; missing legacy
values deserialize to `livekit`. Existing legacy calls can still join their SFU.
Capabilities expose `peer_enabled` and `livekit_enabled`; gate direct/group UI
separately, not just the aggregate `enabled` flag. Peer-only deployments leave
`call.livekit_url` empty and cannot create group calls.

`call.peer.allow_turn` defaults to **false**, independently of `peer.enabled`.
With peer enabled and TURN disabled, startup needs neither TURN URLs nor a TURN
secret; STUN is optional. Join returns empty or STUN-only `ice_servers`, no TURN
username/password, and `credential_expires_at_ms=0` (no credentials, NOT call
expiry). Configured TURN URLs and cached TURN join responses are suppressed while
disabled. Previously issued credentials cannot be revoked by this switch.
Without TURN, direct connectivity may fail on restrictive networks; there is no
silent relay or SFU fallback. Set `allow_turn=true` explicitly to enable relay.

The generated configuration uses `config.example.toml` and includes disabled
`[call]` and `[call.peer]` sections. Reproducible deployment steps (no production
deployment is performed by tests):

1. Generate a fresh private configuration with `privchat generate-config PATH`.
2. If TURN fallback is desired, provision coturn with `use-auth-secret`, a strong `static-auth-secret`, a realm,
   restricted peer address policy, quotas, and publicly reachable listener/relay
   ports. Configure TLS certificates for TURN/TLS where needed. Never allow
   unauthenticated relay or copy the secret into client configuration.
3. Enable `call` and `call.peer`. Leave `allow_turn=false` for direct/STUN-only
   operation. For relay, set `allow_turn=true`, supply TURN URLs, and inject the
   shared secret as `PRIVCHAT_TURN_SECRET` (or `turn_secret_env`). Missing URLs or
   secret fail startup validation in this mode. Placeholder examples:
   `stun:turn.example.invalid:3478`,
   `turn:turn.example.invalid:3478?transport=udp`,
   `turns:turn.example.invalid:5349?transport=tcp`.
4. For groups, additionally set the LiveKit URL and inject its API credentials.
   Apply migrations with `privchat migrate` using the intended database explicitly.
5. Verify per-transport capabilities, direct and forced-relay media connections,
   group SFU connections, expiry/renewal, and authenticated signalling before
   exposing the deployment. A successful signal RPC is not proof of media delivery.

When explicitly enabled, TURN credentials use coturn REST's expiring `expiry:call:user` username and
base64 HMAC-SHA1 password. Defaults are credential TTL 600 seconds (60..3600),
journal TTL 300 seconds (60..900). A new join command on the same winning device
renews credentials; old-command replay does not. Credential expiry is not call
expiry and does not revoke an existing TURN allocation immediately.

`call/peer/signal` accepts raw SDP for offer/answer and a JSON string containing
`candidate`, nullable `sdpMid`, and nonnegative `sdpMLineIndex` for ICE. Only the
initiator sends offers: revision starts at 1 and increments exactly once per
restart. Answers/ICE match the current revision. The callee cannot send a restart
offer; any restart-request coordination must use a separately agreed contract.
Every read/write checks live membership, blacklist, lease, joined participant and
winning device. Limits: SDP 64 KiB, ICE 8 KiB, 512 retained events per call,
60 writes and 40 reads per user per 10-second window. Errors never include payloads.

The signal response contains that command's event and its sequence; never advance
the receive cursor from a send response. `call/peer/events` returns only the other
user's current-revision events and a global `next_seq` cursor. It reports expired
journal gaps rather than silently dropping them; after an initiator restart,
`after_seq=0` retrieves the retained current negotiation. Signal deduplication is
bounded by the journal TTL; terminal calls reject signalling and erase the private
journal/command/rate rows. SDP/ICE is not copied into chat messages or durable
control-command responses. No SFU token or room cleanup is generated for peer calls.

Official TURN credential format: https://github.com/coturn/coturn/blob/master/README.turnserver

Calls are disabled unless `[call] enabled=true` and at least one transport is
configured. LiveKit requires a `ws://` or `wss://` URL and its API credentials;
peer permits direct/STUN-only mode by default, with optional TURN as above. Invalid required credentials fail
closed. Never put secrets in a shared config or client bundle.

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

`call/state` uses a direct per-user authenticated-session one-way `RpcRequest`
(business type 17), with route `call/state` and JSON body `{ "call": CallSnapshot }`.
It uses neither Publish nor chat subscriptions. Shared notification bounds are
256 route bytes, 256 KiB body, and 1024 bytes envelope overhead. Events
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
password login and authentication, one-way RPC notification without subscription
(and explicitly rejects Publish or request/response delivery for call state),
active-list reconciliation, device contention, authenticated peer signal relay,
replay and terminal journal cleanup,
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

Set `PRIVCHAT_CALL_IM_WS_URL` to the fixture's loopback WebSocket URL. The explicit
`PRIVCHAT_CALL_TURN_CHECK=1` option additionally passes the IM-issued TURN credentials
to the local coturn test client on port 56888, checks successful allocation and
actual received UDP packets, and suppresses its address-bearing output. It requires
the isolated loopback coturn fixture and `/opt/homebrew/bin/turnutils_uclient`;
it does not claim that native audio/video tracks were exercised.
