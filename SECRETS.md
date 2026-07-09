# 密钥与敏感配置管理（P2-07）

## 规则

- **`.env` 永不进 repo。** 用 `.env.example` 作模板：`cp .env.example .env` 后填真实值。
- RS256 JWT 私钥、`server.log`、`storage/`、`.local/` 同样不进 repo（见 `.gitignore`）。
- 不同环境（dev / staging / prod）使用**不同**的 `PRIVCHAT_JWT_SECRET` 与
  `SERVICE_MASTER_KEY`。dev 值绝不能出现在生产。

## ⚠️ 历史遗留：旧 `.env` 已在 git 历史中

2026-07-10 之前 `.env` 曾被跟踪，以下 dev 值仍存在于 git 历史（`git log -- .env`）：

- `PRIVCHAT_JWT_SECRET`
- `SERVICE_MASTER_KEY`
- `DATABASE_URL`（含库凭据）
- `REDIS_URL`

`git rm --cached` 只是停止**未来**跟踪，**不会**从历史移除。因此：

1. **生产环境**：这些值若曾用于（或等于）任何生产密钥，必须**立即轮换**——
   - 轮换 `SERVICE_MASTER_KEY`（改 env 并重启所有 server + 更新调用方 application）；
   - 轮换 JWT 签名密钥（HS256 改 secret；RS256 换 keypair + 更新 JWKS，老 token 全失效，
     等同一次强制重新登录）；
   - 轮换 DB / Redis 凭据。
2. **彻底清历史**（可选，若 repo 会外发）：用 `git filter-repo` 或 BFG 移除历史中的
   `.env`，然后 force-push。需与所有协作者协调（会重写 commit 哈希）。当前仓库为内部
   开发库、值均为 dev，未强制执行——**但生产密钥绝不能走这条老路进历史**。

## Cargo.lock 策略（P2-07 附带项，未在本次改动）

`Cargo.lock` 当前被 `.gitignore`。对**二进制** crate（本 server）业界惯例是**提交**
`Cargo.lock` 以保证可复现构建与依赖审计。改动会引入一次较大 add 且影响全仓依赖策略，
留作独立决定（建议：提交 lock，锁定依赖版本）。
