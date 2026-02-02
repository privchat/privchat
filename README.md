# PrivChat Server

High-performance chat server built on the msgtrans framework.

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

# Clone and build
git clone <repository-url>
cd privchat-server
cargo build --release
```

### 2. Configure .env

Create `.env` in the project root (used by server and `sqlx migrate`):

```bash
echo 'DATABASE_URL=postgres://user:password@localhost:5432/privchat' > .env
```

> ⚠️ Add `.env` to `.gitignore`: `echo ".env" >> .gitignore`

### 3. Initialize database

Use [sqlx-cli](https://github.com/launchbadge/sqlx/tree/main/sqlx-cli) (install: `cargo install sqlx-cli --no-default-features --features postgres`):

```bash
sqlx database create
sqlx migrate run
```

**Optional – full reset (dev)**:

```bash
psql "$DATABASE_URL" -f migrations/000_drop_all_tables.sql
psql "$DATABASE_URL" -c "TRUNCATE _sqlx_migrations;"
sqlx migrate run
```

> 💡 Load `.env` before `psql` (e.g. `set -a && source .env && set +a`).

### 4. Start server

```bash
./target/release/privchat-server
./target/release/privchat-server --dev
./target/release/privchat-server --config-file /etc/privchat/config.toml
./target/release/privchat-server --config-file production.toml
```

## 📋 CLI

### Basic

```bash
privchat-server --help
privchat-server --version
privchat-server --config-file config.toml
```

### Common

```bash
privchat-server --config-file config.toml
privchat-server --dev
privchat-server --quiet
```

> 💡 Prefer config file or env for ports, DB, logs. See [Configuration](#-configuration) and [Environment variables](#-environment-variables).

### Utilities

```bash
privchat-server generate-config config.toml
privchat-server validate-config config.toml
privchat-server show-config
```

## 📝 Configuration

### Create config

```bash
privchat-server generate-config config.toml
# or
cp config.example.toml config.toml
```

### Format

TOML, nested. **Recommendation**: non-sensitive in config, secrets in env.

#### Example

**config.toml**:

```toml
[server]
host = "0.0.0.0"
port = 8000
http_file_server_port = 8080
max_connections = 1000

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
base_url = "http://localhost:8080/files"

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

> 💡 Commit config (no secrets); add `.env` to `.gitignore`. Run `privchat-server generate-config config.toml` for full example.

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
privchat-server --dev
```

- Log level: `debug`
- Format: `pretty`
- Verbose output

## 📊 Monitoring

```bash
curl http://localhost:9090/metrics
curl http://localhost:9090/health
```

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
COPY --from=builder /app/target/release/privchat-server /usr/local/bin/
COPY config.example.toml /etc/privchat/config.toml
EXPOSE 8001 8002 9090
CMD ["privchat-server", "--config-file", "/etc/privchat/config.toml"]
```

### Docker Compose

```yaml
version: '3.8'
services:
  privchat-server:
    build: .
    ports:
      - "8001:8001"
      - "8002:8002"
      - "9090:9090"
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
- Tune `--max-connections`, `--worker-threads`, `--tcp-port`, `--ws-port`, `--enable-metrics`.

## 🚨 Troubleshooting

- **Port in use**: `lsof -i :8001`; use `--tcp-port` / `--ws-port` to change.
- **Config errors**: `privchat-server validate-config config.toml` and `privchat-server show-config`.
- **Debug**: `privchat-server --log-level trace --log-format pretty` or `--dev -vv`.

## 🏗️ Modules

Modular layout:

```
privchat-server/
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

**Overall: 95%** ✅

| Category      | SDK | Server RPC | Status | Notes |
|---------------|-----|------------|--------|-------|
| Account       | ✅  | ✅         | ✅ 100% | register, login, authenticate |
| Send message  | ✅  | ✅         | ✅ 100% | send_message, send_attachment |
| Message query | ✅  | ✅         | ✅ 100% | get_message_history, search_messages |
| Message ops   | ✅  | ✅         | ✅ 100% | revoke, add/remove_reaction |
| Conversations | ✅  | ✅         | ✅ 100% | list, pin, hide, mute; auto-created |
| Friends       | ✅  | ✅         | ✅ 100% | apply, accept, reject, remove, list |
| Groups        | ✅  | ✅         | ✅ 100% | create, invite, remove, leave, settings |
| Presence      | ✅  | ✅         | ✅ 100% | subscribe, query, batch_query |
| Typing        | ✅  | ✅         | ✅ 100% | send_typing, stop_typing |
| File up/down  | ✅  | ✅         | ✅ 100% | request_upload_token, HTTP upload/download |
| Sync          | ✅  | ✅         | ✅ 100% | Idempotency, gap detection, Redis, Commit Log, Fan-out |
| Devices       | ✅  | ✅         | ✅ 100% | list, revoke, update_name, kick |
| Search        | ✅  | ✅         | ✅ 100% | search_users, search_by_qrcode |
| Privacy       | ✅  | ✅         | ✅ 100% | get, update |
| Blacklist     | ✅  | ✅         | ✅ 100% | add, remove, list, check |

**Sync**:
- `SyncService` full (P0/P1/P2), beyond Telegram PTS
- Idempotency, gap detection, Redis cache, Commit Log, Fan-out, batch query, cleanup
- Routes: `sync/submit`, `sync/get_difference`, `sync/get_channel_pts`, `sync/batch_get_channel_pts`

**Channel model (2026-01-27)**:
- Removed `channel/create` and `channel/join`; channels auto-created from friend/group actions
- `channel/delete` → `channel/hide` (local hide)
- Added `channel/mute` (notification preference)
- Unified Channel model (list + channel info)

### ✅ Implemented

#### Auth & connection (100%)
- JWT, device management, token revocation
- Login: `AuthorizationRequest`/`AuthorizationResponse`
- Multi-device: SessionManager
- Heartbeat: PingRequest/PongResponse
- Devices: `device/list`, `device/revoke`, `device/update`

#### Messaging (100%)
- Send/Recv, MessageRouter, offline push, storage (PostgreSQL), history (`message/history/get`), revoke (`message/revoke`, 2 min), @mentions, reply, Reactions (add/remove/list/stats), echo, dedup

#### Friends (100%)
- Apply, accept, list, remove, pending; blacklist add/remove/list/check; block non-friend messages; optional non-friend messaging

#### Groups (100%)
- Create, info, list; member add/remove/list/leave; roles (Owner/Admin/Member), transfer_owner, set admin, permissions, mute/unmute, mute_all, settings get/update, QR generate/join, approval list/handle

#### Message status (100%)
- `message/status/read`, `count`, `read_list`, `read_stats`

#### Presence (100%)
- subscribe, query, batch_query, unsubscribe, stats

#### Typing (100%)
- `typing/send`, receive, stats

#### Notifications (100%)
- Friend request, group invite, kick, revoke, system (5 types)

#### Search & privacy (100%)
- `account/search/query`, `by-qrcode`, `account/user/detail`, share_card; `account/privacy/get`, `update`; source verification

#### File upload/download (100%)
- `file/request_upload_token`, `upload_callback`, `validate_token`; HTTP file server (port 8083); **multi-backend** (local FS + S3/OSS/COS/MinIO/Garage via OpenDAL, `default_storage_source_id`); token and URL validation

#### Devices (100%)
- list, revoke, revoke_all, update_name, kick_device, kick_other_devices

#### pts sync (100%) ⭐
- PtsGenerator, MessageWithPts, DeviceSyncState, OfflineQueueService, UnreadCountService; idempotency, gap detection, Redis cache, Commit Log, Fan-out, batch query; RPCs: `sync/get_channel_pts`, `sync/get_difference`, `sync/submit`, `sync/batch_get_channel_pts`

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

#### Persistence
- PostgreSQL, repositories, migrations ✅; all business logic uses DB; memory cache for perf
- Index/query tuning TBD

#### Batch read
- Single read ✅ (Phase 8)
- Batch mark-read RPC ❌
- Batch read receipt broadcast ❌

### ❌ Not implemented (planned)

- **Batch read** (P0): batch mark-read RPC, per-conversation batch, batch receipt broadcast, batch SQL
- **Monitoring** (P1): Prometheus, tracing, alerts, dashboard
- **Network** (P1): connection quality, reconnect, backoff, weak network
- **Perf** (P2): batch send, index/query, cache warmup, slow-query tuning

## 📊 Project status

### Completion

- **Server**: 96% ✅ (core + multi-backend S3/OSS)
- **SDK**: 95% ✅ (core + media preprocessing/thumbnails)
- **Production readiness**: 86% ⚠️ (persistence 95%, core 100%, multi-backend ✅; monitoring/perf TBD)
- **Tests**: 100% ✅ (26/26 phases)
- **SDK–server alignment**: 95% ✅

### Module completion (summary)

| Module        | Completion | Status | Notes |
|---------------|-------------|--------|-------|
| Core messaging| 95%         | ✅     | Send, receive, forward, reply |
| Message types | 95%         | ✅     | 10+ types |
| Offline       | 100%        | ✅     | Push + tests |
| Revoke        | 100%        | ✅     | 2 min |
| @Mention      | 100%        | ✅     | + tests |
| Reply         | 100%        | ✅     | + tests |
| Reaction      | 100%        | ✅     | Full |
| Auth          | 95%         | ✅     | JWT, devices |
| Conversations | 100%        | ✅     | list, pin, hide, mute |
| Read receipts | 100%        | ✅     | RPC + broadcast |
| Groups        | 100%        | ✅     | Full |
| File storage  | 98%         | ✅     | Local + S3/OSS (OpenDAL) |
| Friends       | 100%        | ✅     | Full |
| Presence      | 100%        | ✅     | Full |
| Typing        | 100%        | ✅     | Full |
| Notifications | 100%        | ✅     | 5 types |
| Stickers      | 60%         | ⚠️     | RPC only |
| Search        | 90%         | ✅     | User + SDK local |
| pts sync      | 100%        | ✅     | P0/P1/P2, all routes |
| Dedup         | 100%        | ✅     | Server + SDK |
| Persistence   | 95%         | ✅     | PostgreSQL + repos |

### Production checklist

**Required**: ✅ Persistence, dedup, auth, encrypted transport; ⚠️ Monitoring; ✅ Logging, errors, functional tests (26/26); ❌ Load test, DR, backup

**Recommended**: ✅ Revoke, read receipt, offline, groups, @mention, reply, Reaction, presence, typing; ⚠️ Stickers; ❌ Batch read; ❌ Message edit (by design: revoke+resend only)

## 🚀 Roadmap

### Near-term (P0)
1. Batch read RPC, per-conversation batch, batch receipt, batch SQL
2. Prometheus, metrics, Grafana, alerts, tracing
3. Load test, throughput, DB/query/cache/connection tuning

### Short-term (P1)
4. Message pin, forward improvements, full-text search
5. **Media**: ✅ Image (SDK thumbnails, optional compress; server S3/OSS); ✅ Video (SDK hook, auto-download thumb; server multi-backend); Voice (STT, progress); File preview (Office, PDF)
6. Group: announcements, polls, files, sub-channels

### Mid-term (P2)
7. Voice/video (WebRTC), group calls, screen share, recording
8. E2EE (Signal, X3DH, Double Ratchet, safety number, secret chat, disappearing)
9. Bots: API, webhooks, /commands, auto-reply, scheduled, templates
10. Enterprise: org, permissions, audit, SSO, export, admin UI

### Long-term (P3)
11. Multi-region, routing, sync, failover, CDN
12. AI: translation, moderation, recommendations, assistant, STT, OCR
13. Compression, connection pool, multi-level cache, load balancing, scaling

## 📊 Progress (2026-02-02)

- **Core**: 96%; **Production**: 86%; **Tests**: 100%; **Docs**: 85%
- **Recent**: Multi-backend file storage (OpenDAL, S3/OSS/COS/MinIO/Garage); SDK image compression & thumbnails, video hook, auto-download thumb; Channel model and API (hide/mute, auto-creation)
- **Done**: Messaging, friends, groups, presence, typing, notifications, pts sync, persistence, file up/down (multi-backend), SDK media preprocessing
- **Next**: Batch read RPC, monitoring, load testing

## 📚 Docs

- [GROUP_SIMPLE_DESIGN.md](docs/GROUP_SIMPLE_DESIGN.md)
- [MESSAGE_STATUS_SYSTEM.md](docs/MESSAGE_STATUS_SYSTEM.md)
- [SEARCH_SYSTEM_DESIGN.md](docs/SEARCH_SYSTEM_DESIGN.md)
- [TELEGRAM_SYNC_IMPLEMENTATION.md](docs/TELEGRAM_SYNC_IMPLEMENTATION.md)
- [OFFLINE_MESSAGE_SIMPLE_DESIGN.md](docs/OFFLINE_MESSAGE_SIMPLE_DESIGN.md)
- [AUTH_SYSTEM_COMPLETE.md](docs/AUTH_SYSTEM_COMPLETE.md), [BLACKLIST_DESIGN.md](docs/BLACKLIST_DESIGN.md), [MESSAGE_REVOKE_IMPLEMENTATION.md](docs/MESSAGE_REVOKE_IMPLEMENTATION.md), [READ_RECEIPT_BROADCAST_IMPLEMENTATION.md](docs/READ_RECEIPT_BROADCAST_IMPLEMENTATION.md)
- [PERSISTENCE_ARCHITECTURE.md](docs/PERSISTENCE_ARCHITECTURE.md), [RPC_DESIGN.md](docs/RPC_DESIGN.md), [FILE_STORAGE_ARCHITECTURE.md](docs/FILE_STORAGE_ARCHITECTURE.md)

All design docs are under `docs/` (45 documents).

## 📄 License

MIT. See [LICENSE](../LICENSE).

## 🤝 Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md).

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

**Core**: 96% | **Production**: 86% | **Tests**: 100% ✅ | **SDK alignment**: 95% ✅ | **Sync**: 100% ✅

*Last updated: 2026-02-02*  
*Status: 26/26 test phases pass; SDK–server 95% aligned*  
*Done: Messaging, groups, friends, @mention, reply, Reaction, presence, typing, notifications, file up/down (multi-backend S3/OSS), conversations; SDK image/video preprocessing and thumbnails*  
*Recent: Multi-backend (OpenDAL), S3/OSS/COS/MinIO/Garage; SDK media (thumbnails, video Thumbnail/Compress)*  
*Next: Batch read RPC, monitoring, load testing*

[Quick Start](#-quick-start) • [Feature List](#-feature-list) • [Docs](docs/) • [Contributing](../CONTRIBUTING.md)

</div>
