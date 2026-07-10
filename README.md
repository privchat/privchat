# PrivChat

A high-performance, secure instant messaging server built with Rust.

## 🎯 Vision

PrivChat aims to build a **secure, high-performance, feature-complete** modern instant messaging system with a Telegram-like user experience while keeping the architecture simple and maintainable.

### Core Goals

- 🔒 **Privacy First**: End-to-end encryption, full privacy controls, data security
- ⚡ **Extreme Performance**: Million-scale concurrency, millisecond latency, high-availability architecture
- 🌍 **Global Reach**: Multi-region deployment, smart routing, edge access
- 🎨 **User Friendly**: Intuitive, feature-rich, smooth experience
- 🔧 **Developer Friendly**: Clear architecture, complete documentation, easy to extend

### Design Principles

1. **Local-First Architecture**: Client-first, offline-capable, incremental sync
2. **Actor Model**: Isolated concurrency, consistency guarantees, reduced complexity
3. **Multi-Protocol Support**: Unified abstraction for TCP/WebSocket/QUIC
4. **Modular Design**: Clear layering, single responsibility, testable
5. **Production Ready**: Full monitoring, logging, disaster recovery
6. **Security First**: Mitigate attacks, protect user assets, learn from WeChat’s security design

### Security Design Principle

#### 🚫 No Message Editing

**Security rationale**:
- ❌ **Telegram lesson**: Message editing was abused to alter USDT addresses and steal user funds
- ❌ **Trust**: Editable history undermines authenticity and immutability
- ❌ **Financial risk**: Editing is a major risk for transfers, payments, contracts

**Alternative** (inspired by WeChat):
- ✅ **Revoke + Resend**: Users can revoke messages (within 2 minutes), edit locally, then resend
- ✅ **Explicit marker**: Revoke leaves a system line like “XXX recalled a message”
- ✅ **Time limit**: 2-minute revoke window to prevent long-term tampering
- ✅ **Audit trail**: Server may keep audit logs of revoked messages (enterprise)

**Philosophy**:
> Message immutability matters more than edit convenience. Prefer revoke-and-resend over giving attackers a way to tamper.

## ✨ Features

### Core

- 🚀 **High performance**: 1M+ concurrent connections, sub-ms message latency
- 🌐 **Multi-protocol**: TCP, WebSocket, QUIC
- 🔧 **Flexible config**: CLI args, config file, env vars
- 📊 **Monitoring**: Prometheus metrics and health checks
- 🛡️ **Security**: JWT auth, rate limiting, CORS
- 🔄 **Graceful shutdown**: Signal handling and clean teardown

### Business Features

- 💬 **Messaging**: Text, image, audio, video, file, location, and more
- 👥 **Friends**: Request, accept, list, remove, blacklist
- 👨‍👩‍👧‍👦 **Groups**: Create, members, roles, mute, approval, QR join
- 📱 **Multi-device sync**: Telegram-style pts sync, multi-device presence
- 📨 **Offline messages**: Event-driven push, sync on connect
- ✅ **Read receipts**: Per-chat and per-group read state, lists, stats
- 📋 **Conversations**: List, pin, hide, mute; unified Channel model; auto-creation
- 🔄 **Message revoke**: Within 2 minutes (sender, owner, admin); replaces editing
- 🔍 **Search**: User search, message history, QR search
- 🔒 **Privacy**: Privacy settings, non-friend message control, source verification
- 📁 **Files**: Upload, download, token management, URL validation; **multi-backend** (local + S3/OSS/COS/MinIO/Garage via OpenDAL)
- 🎨 **Stickers**: Sticker messages (storage TBD)
- 📊 **Deduplication**: Server and client dedup for idempotency

## 🚀 Quick Start

### 1. Dependencies

```bash
# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build (the produced binary is named `privchat`)
git clone <repository-url>
cd privchat
cargo build --release          # → ./target/release/privchat
```

> **Migrations are embedded into the binary at build time.** `build.rs`
> scans `migrations/*.sql` (skipping `000_`-prefixed reset scripts), sorts
> them by filename, and bakes them into a `MIGRATIONS` constant. Adding a
> new migration therefore requires a rebuild before `privchat migrate` can
> apply it — there is no runtime SQL-directory read.

### 2. Configure .env

Copy the template and fill in real values (`.env` is git-ignored; see
[`SECRETS.md`](SECRETS.md) for the full key list and rotation rules):

```bash
cp .env.example .env
# edit .env: DATABASE_URL, PRIVCHAT_JWT_SECRET, SERVICE_MASTER_KEY, REDIS_URL
```

Hard-required (startup aborts if any is empty — see `Config::validate`):
`DATABASE_URL`, `SERVICE_MASTER_KEY`, `REDIS_URL`. JWT signing config is also
needed for auth — `PRIVCHAT_JWT_SECRET` for HS256, or
`PRIVCHAT_JWT_PRIVATE_KEY_PATH` / `PRIVCHAT_JWT_PUBLIC_KEY_PATH` for RS256
(production uses RS256 + JWKS).

### 3. Initialize database

Schema changes run through the binary's own `migrate` subcommand — **not**
sqlx-cli. It connects with `DATABASE_URL`, creates a `privchat_migrations`
tracking table, and applies every embedded migration not yet recorded:

```bash
./target/release/privchat migrate
```

This is the **only** supported path for schema changes; never apply SQL by
hand in production.

**Optional – full reset (dev only)**:

```bash
set -a && source .env && set +a                              # load DATABASE_URL for psql
psql "$DATABASE_URL" -f migrations/000_drop_all_tables.sql   # 000_ is skipped by migrate; dev reset only
psql "$DATABASE_URL" -c "DROP TABLE IF EXISTS privchat_migrations;"
./target/release/privchat migrate
```

### 4. Start server

```bash
./target/release/privchat                                      # default config.toml
./target/release/privchat --dev
./target/release/privchat --config-file /etc/privchat/config.toml
```

Graceful shutdown: `SIGINT` (Ctrl-C) or `SIGTERM` stops accepting new
connections, flushes pending presence writes, then exits cleanly (P1-11).

## 📋 CLI

### Basic

```bash
privchat --help
privchat --version
privchat --config-file config.toml
```

### Common

```bash
privchat --config-file config.toml
privchat --dev
privchat --quiet
```

> 💡 Prefer config file or env for ports, DB, logs. See [Configuration](#-configuration) and [Environment variables](#-environment-variables).

### Utilities

```bash
privchat generate-config config.toml
privchat validate-config config.toml
privchat show-config
```

## 📝 Configuration

### Create config

```bash
privchat generate-config config.toml
# or
cp config.example.toml config.toml
```

### Format

TOML, nested. **Recommendation**: non-sensitive in config, secrets in env.

**Config split**: Gateway uses `[gateway]` with a **listeners array** (`[[gateway.listeners]]`). File storage and HTTP file service use `[file]` (including `server_port`, `server_api_base_url`). Future: `[msg_server]` etc.

**Default port spec (PrivChat)**:

| Port | Protocol | Purpose |
|------|----------|---------|
| **9001** | TCP/UDP | Gateway (msgtrans/QUIC); same port for TCP and QUIC |
| **9080** | TCP | WebSocket Gateway (dev); prod may use 9443/wss |
| **9083** | TCP | File HTTP (upload/download + Admin API) |
| **9090** | TCP | Reserved for Admin API (currently shared with 9083) |

Design: **900x** = core gateway, **908x** = HTTP/web, **909x** = admin; avoid 8080-style web semantics.

#### Example

**config.toml**:

```toml
# Gateway: listeners array (production-style)
[gateway]
max_connections = 100000
connection_timeout = 300
heartbeat_interval = 60
use_internal_auth = true

# TCP/QUIC same port 9001
[[gateway.listeners]]
protocol = "tcp"
host = "0.0.0.0"
port = 9001

[[gateway.listeners]]
protocol = "quic"
host = "0.0.0.0"
port = 9001

[[gateway.listeners]]
protocol = "websocket"
host = "0.0.0.0"
port = 9080
path = "/gate"
compression = true

# File: storage + HTTP file service (upload/download + Admin API)
[file]
server_port = 9083
server_api_base_url = "http://localhost:9083/api/app"
default_storage_source_id = 0
# [[file.storage_sources]] ...

[cache]
l1_max_memory_mb = 256
l1_ttl_secs = 3600

# Redis: use env REDIS_URL
# [cache.redis]
# url = "from REDIS_URL"
# pool_size = 10

[file]
default_storage_source_id = 0
# At least one [[file.storage_sources]]; supports local and s3 (S3/OSS/COS/MinIO/Garage)
[[file.storage_sources]]
id = 0
storage_type = "local"
storage_root = "./storage/files"
base_url = "http://localhost:9083/files"

[logging]
level = "info"
format = "compact"
```

**.env** (sensitive):

```bash
DATABASE_URL=postgresql://user:password@localhost/privchat
REDIS_URL=redis://127.0.0.1:6379
PRIVCHAT_JWT_SECRET=your-super-secret-key-minimum-64-chars
```

#### Priority

1. Config file: non-sensitive (ports, paths, log level)
2. Env vars: secrets; override config
3. Precedence: env > config > defaults
4. `.env` is loaded automatically when present

> 💡 Commit config (no secrets); add `.env` to `.gitignore`. Run `privchat generate-config config.toml` for full example. Real config also includes `[system_message]`, `[security]`, etc.; see `config.example.toml`.

### Config precedence

1. **CLI** (highest)
2. **Env** (prefix `PRIVCHAT_`)
3. **Config file**
4. **Defaults** (lowest)

## 🌍 Environment Variables

All options can be set via env with `PRIVCHAT_` prefix. **`.env` is auto-loaded** when present.

### .env example

```bash
DATABASE_URL=postgresql://user:password@localhost/privchat
REDIS_URL=redis://localhost:6379
PRIVCHAT_JWT_SECRET=your-super-secret-key-minimum-64-chars
PRIVCHAT_HOST=0.0.0.0
PRIVCHAT_LOG_LEVEL=info
PRIVCHAT_LOG_FORMAT=json
```

> ⚠️ Add `.env` to `.gitignore`.

### Manual export

```bash
export PRIVCHAT_HOST=0.0.0.0
export DATABASE_URL="postgresql://user:pass@localhost/privchat"
export REDIS_URL="redis://localhost:6379"
export PRIVCHAT_JWT_SECRET="your-super-secret-key-minimum-64-chars"
```

> 💡 Env overrides config; use `.env` or container env in production.

## 🔧 Dev mode

```bash
privchat --dev
```

- Log level: `debug`
- Format: `pretty`
- Verbose output

## 📊 Monitoring

- **Health**: RPC `system/health` (no auth); no dedicated HTTP endpoint.
- **Prometheus**: GET `/metrics` on the file server port (default **9083**), exposing:
  - `privchat_connections_current`, `privchat_rpc_total{route}`, `privchat_rpc_duration_seconds{route}`, `privchat_messages_sent_total`.
- Scrape example: `curl http://localhost:9083/metrics`; configure Prometheus and Grafana as needed.
- For HTTP health, extend routes on port 9083.

### Admin API

Same port as the file HTTP server (config `file.server_port`, default **9083**), authenticated with `X-Service-Key`:

- `POST/GET /api/service/users`, `/api/service/users/{user_id}` – user management
- `GET/DELETE /api/service/groups`, `/api/service/groups/{group_id}` – group management
- `POST/GET /api/service/friendships`, `/api/service/friendships`, `/api/service/friendships/user/{user_id}` – friendships
- `GET /api/service/login-logs`, `/api/service/devices`, `/api/service/stats/*`, `/api/service/messages`, etc.

See `scripts/README.md` and `scripts/test_admin_api.py`.

## 🛡️ Security (production)

- Use config file for most settings; env for secrets.
- Set `PRIVCHAT_JWT_SECRET` with e.g. `openssl rand -hex 32`.
- Keep `.env` out of version control.

## 🐳 Docker

### Dockerfile

```dockerfile
FROM rust:1.90 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates
COPY --from=builder /app/target/release/privchat /usr/local/bin/
COPY config.example.toml /etc/privchat/config.toml
EXPOSE 9001 9080 9083
CMD ["privchat", "--config-file", "/etc/privchat/config.toml"]
```

### Docker Compose

```yaml
version: '3.8'
services:
  privchat:
    build: .
    ports:
      - "9001:9001"   # TCP/QUIC Gateway
      - "9080:9080"   # WebSocket Gateway
      - "9083:9083"   # HTTP file service (upload/download + Admin API)
    environment:
      - DATABASE_URL=postgresql://user:pass@postgres/privchat
      - REDIS_URL=redis://redis:6379
      - PRIVCHAT_JWT_SECRET=${PRIVCHAT_JWT_SECRET}
    depends_on:
      - postgres
      - redis

  postgres:
    image: postgres:17
    environment:
      - POSTGRES_DB=privchat
      - POSTGRES_USER=user
      - POSTGRES_PASSWORD=pass

  redis:
    image: redis:7-alpine
```

## 📈 Performance

- Increase `fs.file-max` and `net.core.somaxconn` as needed.
- Tune `--max-connections`, `--worker-threads`, `--tcp-port 9001`, `--ws-port 9080`, `--enable-metrics`.

## 🚨 Troubleshooting

- **Port in use**: `lsof -i :9001` (or `:9080`); use `--tcp-port` / `--ws-port` to change.
- **Config errors**: `privchat validate-config config.toml` and `privchat show-config`.
- **Debug**: `privchat --log-level trace --log-format pretty` or `--dev -vv`.

## 🏗️ Modules

Modular layout:

```
privchat/
├── 🔐 auth         - JWT, device management, token revocation
├── 💬 message      - send, receive, route, store
├── 👥 contact      - friends, requests, blacklist
├── 👨‍👩‍👧‍👦 group        - groups, members, permissions
├── 🔍 search       - user/message search, privacy
├── 💾 storage      - PostgreSQL, repositories
├── 📦 cache        - L1 memory, L2 Redis
├── 📁 file         - upload, tokens, URL validation; multi-backend (local + S3/OSS/COS/MinIO, OpenDAL)
├── 🔔 notification - system notifications, push
├── 👁️ presence     - online/typing, subscriptions
├── 🔒 privacy      - settings, source verification
├── 🛡️ security     - rate limit, IP blacklist
├── 📊 monitoring   - Prometheus, health
└── 🚀 transport   - TCP/WebSocket/QUIC
```

### Stack

| Layer    | Tech              | Notes                    |
|----------|-------------------|--------------------------|
| Transport| msgtrans          | TCP/WS/QUIC              |
| App      | Tokio + Async Rust| Concurrency              |
| Storage  | PostgreSQL + sqlx | Consistency              |
| Cache    | Redis + memory    | Two-level                |
| Queue    | Redis Pub/Sub     | Events                   |
| Monitoring | Prometheus + Tracing | Metrics, tracing   |

## 📋 Feature List

### SDK vs server alignment

**Current alignment: high, but not a blanket 100%.**

The core SDK-facing IM surface is implemented, while a few endpoints are still
partial or intentionally delegated to client/local storage. The table below is
kept conservative and reflects the current server code rather than the older
"26/26 phases" status.

| Category      | SDK | Server RPC | Status | Notes |
|---------------|-----|------------|--------|-------|
| Account       | ✅  | ✅         | Stable | register, login, refresh, unified token API, device sessions |
| Send message  | ✅  | ✅         | Stable | send, attachments, echo, dedup, persistence |
| Message query | ✅  | ✅         | Stable | history, read state, entity sync; full-text search still planned |
| Message ops   | ✅  | ✅         | Stable | revoke, add/remove/list/stats reactions |
| Conversations | ✅  | ✅         | Stable | entity/sync_entities + local get_channels; pin, hide, mute, direct/get_or_create |
| Friends       | ✅  | ✅         | Stable | apply, accept, reject, recall, remove, alias, tombstone sync |
| Groups        | ✅  | ✅         | Stable | create, members, roles, settings, QR join, approval |
| Presence      | ✅  | ✅         | Stable | current status and realtime `presence_changed` delivery |
| Typing        | ✅  | ✅         | Stable | send typing with server-side rate limit |
| File up/down  | ✅  | ✅         | Mostly done | upload token, HTTP upload/download, local + S3-compatible backends; quota/callback enrichment pending |
| Sync          | ✅  | ✅         | Mostly done | pts, idempotency, gap detection, Commit Log, cache, fan-out; permission check cleanup still pending |
| Devices       | ✅  | ✅         | Stable | list, revoke, revoke_all, update_name, kick |
| Search        | ✅  | ✅         | Mostly done | user and QR search; advanced/full-text message search pending |
| Privacy       | ✅  | ✅         | Stable | get/update privacy settings and source validation |
| Blacklist     | ✅  | ✅         | Stable | add, remove, list, check |
| QR login      | ✅  | ✅         | Stable | unauth scene creation, scan/confirm/reject state machine, push pipeline |
| Bot follow    | ✅  | ✅         | Stable | account/bot/follow and unfollow, relation persistence, server-event emit |
| Channel transfer | ✅ | ✅      | Stable | wire-layer transfer relay via server-event dispatch |
| Push          | ✅  | ✅         | Mostly done | planner/worker with APNS, FCM, HMS, Xiaomi, Oppo, Vivo, Honor, Lenovo, ZTE, Meizu |

**Sync**:
- Active routes: `sync/submit`, `sync/get_difference`, `sync/get_channel_pts`,
  `sync/batch_get_channel_pts`, `sync/session_ready`
- Implemented: pts allocation, idempotency, gap detection, Commit Log, cache,
  fan-out, batch query
- Remaining hardening: channel permission validation, online-user cache cleanup,
  and integration-test refresh

**Recent server-side additions after the old README snapshot**:
- Unified service token API: `/api/service/auth/{issue,refresh,introspect,revoke}` + JWKS
- Web/PC QR login: unauth RPC `qr_login/create_scene` and HTTP scan/confirm/reject flow
- Permanent user/group QR keys and QR URL builder
- Bot follow/unfollow and generic server-event dispatch
- Room subscribe tickets and channel transfer send endpoint
- Multi-vendor offline push provider pipeline
- Prometheus `/metrics` endpoint with core connection/RPC/delivery counters

**Channel model**:
- Removed `channel/create` and `channel/join`; channels auto-created from friend/group actions
- `channel/delete` → `channel/hide` (local hide)
- Added `channel/mute` (notification preference)
- Unified Channel model (list + channel info)

### ✅ Implemented

#### Auth & connection
- JWT, refresh tokens, device management, token revocation, unified service token API
- Login: `AuthorizationRequest`/`AuthorizationResponse`; HTTP service token endpoints
- Multi-device: SessionManager
- Heartbeat: PingRequest/PongResponse; unauth connecting-session watchdog
- Devices: `device/list`, `device/revoke`, `device/update`

#### Messaging
- Send/Recv, MessageRouter, offline push, storage (PostgreSQL), history (`message/history/get`), revoke (`message/revoke`, 2 min), @mentions, reply, Reactions (add/remove/list/stats), echo, dedup

#### Friends
- Apply, accept, list, remove, pending; blacklist add/remove/list/check; block non-friend messages; optional non-friend messaging

#### Groups
- Create, info, list; member add/remove/list/leave; roles (Owner/Admin/Member), transfer_owner, set admin, permissions, mute/unmute, mute_all, settings get/update, QR generate/join, approval list/handle

#### Message status
- `message/status/read_pts`, `count`, `read_list`, `read_stats`

#### Presence
- subscribe, query, batch_query, unsubscribe, stats

#### Typing
- `typing/send`, receive, stats

#### Notifications and push
- Friend request, group invite, kick, revoke, system (5 types)
- Push planner/worker with Redis online check and multi-vendor providers

#### Search & privacy
- `account/search/query`, `by-qrcode`, `account/user/detail`, share_card; `account/privacy/get`, `update`; source verification

#### File upload/download
- `file/request_upload_token`, `upload_callback`, `validate_token`; HTTP file server (port **9083**); **multi-backend** (local FS + S3/OSS/COS/MinIO/Garage via OpenDAL, `default_storage_source_id`); token and URL validation

#### Devices
- list, revoke, revoke_all, update_name, kick_device, kick_other_devices

#### pts sync
- PtsGenerator, MessageWithPts, DeviceSyncState, OfflineQueueService, UnreadCountService; idempotency, gap detection, Redis cache, Commit Log, Fan-out, batch query; RPCs: `sync/get_channel_pts`, `sync/get_difference`, `sync/submit`, `sync/batch_get_channel_pts`

#### QR, service API, and server integration
- QR login scene lifecycle and unauth push pipeline
- User/group QR key persistence and URL builder
- Bot follow/unfollow with server-event notification
- Channel Transfer wire-layer utilities and `/api/service/transfer/send`
- Room subscribe ticket issuer
- Generic server-event envelope for downstream application modules

### ⚠️ Partial

#### Message types
- Basic: Text, Image, Audio, Video, File, Location, System; design done (forward, batch forward, placeholders)
- Payload: currently string; need content + metadata
- Advanced types (cards, stickers, red packets, transfer, H5 share): not implemented

#### File storage
- Token management, URL validation ✅
- **Multi-backend**: local FS + S3/OSS/COS/MinIO/Garage (OpenDAL, `[[file.storage_sources]]`) ✅
- Stickers: RPC done, storage TBD
- **Image compression & thumbnails**: SDK (default thumbnail, video hook Thumbnail/Compress, auto-download thumb on receive) ✅
- Upload callback, quotas, media post-processing and content review hooks still need hardening

#### Persistence
- PostgreSQL, repositories, migrations ✅; all business logic uses DB; memory cache for perf
- Index/query tuning TBD

#### Sync and offline hardening
- `sync/submit` still needs explicit channel permission validation
- Online-user Redis removal and expired-offline-message cleanup need completion
- Some older integration tests and examples still need protocol-shape updates

### ❌ Not implemented (planned)

- **Monitoring** (P1): Grafana dashboard, alerts, tracing
- **Network** (P1): connection quality, reconnect, backoff, weak network
- **Perf** (P2): batch send, index/query, cache warmup, slow-query tuning
- **Production ops** (P2): load test, backup, restore, DR runbooks

## 📊 Project status

### Completion

- **Server feature coverage**: high, roughly mid/high-90s for core IM features; not a verified 99%
- **SDK**: 95% ✅ (core + media preprocessing/thumbnails)
- **Production readiness**: in progress (monitoring basics done; load test, alerting, DR and backup still pending)
- **Tests**: library unit tests pass (`cargo test --lib`: 241 passed, 3 ignored); full `cargo test --no-fail-fast` currently needs test/example updates after protocol and transfer refactors
- **SDK–server alignment**: high for core workflows, with advanced search/media/sticker/storage gaps still tracked

### Module completion (summary)

| Module        | Status | Notes |
|---------------|--------|-------|
| Core messaging| Stable | Send, receive, revoke, mention, reply, reaction, dedup |
| Message types | Mostly done | Common types supported; richer payload parsing and advanced types pending |
| Offline       | Mostly done | Queue and push path exist; cleanup and load verification pending |
| Auth          | Stable | JWT, refresh token repository, devices, unified service token API |
| Conversations | Stable | entity sync, direct get/create, pin, hide, mute |
| Read receipts | Stable | `read_pts`, count, list, stats |
| Groups        | Stable | Members, roles, settings, QR join, approval |
| File storage  | Mostly done | Local + S3-compatible OpenDAL; quota/callback hooks pending |
| Friends       | Stable | Request lifecycle, alias, blacklist, tombstone sync |
| Presence      | Stable | User aggregation and realtime delivery |
| Push          | Mostly done | Planner/worker + multi-vendor providers; operational validation pending |
| QR login      | Stable | Scene state machine and unauth push pipeline |
| Bot follow    | Stable | Follow relation and server-event emit |
| Channel transfer | Stable | Wire validation and server-event dispatch path |
| Stickers      | Partial | Package/list RPC exists; durable storage still TBD |
| Search        | Mostly done | User/QR search; full-text message search pending |
| pts sync      | Mostly done | Core path works; permission validation and test refresh pending |
| Persistence   | Mostly done | PostgreSQL + repositories + migrations; index/query tuning pending |

### Production checklist

**Ready or mostly ready**: persistence, dedup, auth/session/device management, structured logging, `/metrics`, core IM workflows, QR login, transfer, Push pipeline.

**Still required before production sign-off**: full integration test refresh, load testing, Grafana/alerts/tracing, backup/restore, DR plan, sync permission validation, offline cleanup.

**By design**: message editing is not implemented. Use revoke-and-resend.

## 🚀 Roadmap

### Near-term (P0)
1. Refresh failing tests/examples after FlatBuffers and transfer refactors
2. Finish sync permission validation and online/offline cleanup tasks
3. Add Grafana dashboard, alerts, tracing, and production scrape examples
4. Run load tests and tune DB/query/cache/connection pools

### Short-term (P1)
5. Message pin, forward improvements, full-text search
6. **Media**: ✅ Image (SDK thumbnails, optional compress; server S3/OSS); ✅ Video (SDK hook, auto-download thumb; server multi-backend); Voice (STT, progress); File preview (Office, PDF)
7. Group: announcements, polls, files, sub-channels

### Mid-term (P2)
8. Voice/video (WebRTC), group calls, screen share, recording
9. E2EE (Signal, X3DH, Double Ratchet, safety number, secret chat, disappearing)
10. Bots: API, webhooks, /commands, auto-reply, scheduled, templates
11. Enterprise: org, permissions, audit, SSO, export, admin UI

### Long-term (P3)
12. Multi-region, routing, sync, failover, CDN
13. AI: translation, moderation, recommendations, assistant, STT, OCR
14. Compression, connection pool, multi-level cache, load balancing, scaling

## 📊 Progress (2026-05-28)

- **Core feature coverage**: high; core IM workflows, multi-device, persistence, sync, file storage, QR login, bot follow, transfer, Push and service APIs are present.
- **Production readiness**: still in progress; monitoring basics exist, but alerting, tracing, load testing, backup and DR need completion.
- **Tests**: `cargo test --lib` passes; full test suite currently has stale integration tests/examples after protocol and transfer refactors.
- **Recent**: unified token API, QR login, user/group QR keys, server-event dispatch, bot follow, room tickets, channel transfer, multi-vendor Push.
- **Next**: test refresh, sync/offline hardening, observability, load testing.

## 📚 Docs

Design docs live under **privchat-docs** or **privchat/docs**; see repo layout.

- [OFFLINE_SPEC.md](../privchat-docs/spec/02-server/OFFLINE_SPEC.md)
- [SEARCH_SPEC.md](../privchat-docs/spec/05-feature/SEARCH_SPEC.md)
- [DEPLOYMENT_SPEC.md](../privchat-docs/spec/06-ops/DEPLOYMENT_SPEC.md)
- Legacy design notes are archived under `../privchat-docs/_archive/legacy/` and `../privchat-docs/_archive/design-iterations/`.
- Auth, blacklist, message revoke, read receipt, persistence, RPC, file storage – under `privchat-docs` or `docs/`

## 📄 License

MIT. See [LICENSE](LICENSE).

## 🤝 Contributing

Contribution guide is not yet checked into this package. Please open an issue or follow the repository conventions for now.

## 📞 Support

- [GitHub Issues](https://github.com/privchat/privchat/issues)
- [zoujiaqing@gmail.com](mailto:zoujiaqing@gmail.com)

---

## 🌟 Credits

- [Tokio](https://tokio.rs/) – async runtime
- [MsgTrans](https://github.com/zoujiaqing/msgtrans) – transport
- [sqlx](https://github.com/launchbadge/sqlx) – database
- [tracing](https://github.com/tokio-rs/tracing) – logging
- [serde](https://serde.rs/) – serialization

---

<div align="center">

**PrivChat Server** – Faster, more stable, more secure chat 🚀

[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Test Coverage](https://img.shields.io/badge/coverage-95%25-brightgreen)]()
[![License](https://img.shields.io/badge/license-MIT-blue)]()
[![Rust Version](https://img.shields.io/badge/rust-1.90%2B-orange)]()

**Core**: high coverage | **Production**: in progress | **Tests**: lib tests pass, full suite needs refresh | **SDK alignment**: high

*Last updated: 2026-05-28*
*Status: core library tests pass; integration tests/examples need refresh after protocol and transfer refactors*
*Done: Messaging, groups, friends, @mention, reply, Reaction, presence, typing, notifications, file up/down, conversations, QR login, bot follow, transfer, Push*
*Recent: unified token API, QR keys, server-event dispatch, room tickets, multi-vendor Push*
*Next: test refresh, sync/offline hardening, observability, load testing*

[Quick Start](#-quick-start) • [Feature List](#-feature-list) • [Docs](../privchat-docs/spec)

</div>
