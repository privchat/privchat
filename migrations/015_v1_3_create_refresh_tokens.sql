-- 015: v1.3 unified refresh token records
-- 目标：
--   v1.2 的 refresh token 是 stateless HS256 JWT，无 record 表，单 token 无法 revoke。
--   v1.3 Phase A 引入 unified token 体系（spec TOKEN_UNIFICATION_SPEC §11.2），
--   refresh token 仍走 RS256 JWT 颁发，但 server 端按 jti + token_hash 持久化记录，
--   支持单 token revoke 与设备级失效审计。
--
-- 约束：
--   - token_hash 永远不存明文（SHA-256 hex of refresh JWT）
--   - 验证流程：解 JWT -> jti -> 查表 -> 比对 hash -> 检查 revoked_at IS NULL 且 expires_at > now
--   - 设备级失效仍走 privchat_devices.session_version bump，与本表互补
--
-- 兼容：
--   - 本表只服务 v1.3 unified refresh token 路径
--   - v1.2 的 stateless refresh JWT 行为保持不变（不 backfill；旧 token 自然过期）

CREATE TABLE IF NOT EXISTS privchat_refresh_tokens (
    jti              VARCHAR(64) PRIMARY KEY,
    user_id          BIGINT      NOT NULL,
    device_id        VARCHAR(128) NOT NULL,
    token_hash       VARCHAR(64) NOT NULL UNIQUE,
    session_version  BIGINT      NOT NULL,
    expires_at       BIGINT      NOT NULL,
    revoked_at       BIGINT,
    revoke_reason    VARCHAR(128),
    created_at       BIGINT      NOT NULL,
    last_used_at     BIGINT
);

CREATE INDEX IF NOT EXISTS idx_privchat_refresh_tokens_user_device
    ON privchat_refresh_tokens (user_id, device_id);

CREATE INDEX IF NOT EXISTS idx_privchat_refresh_tokens_expires_at
    ON privchat_refresh_tokens (expires_at);

CREATE INDEX IF NOT EXISTS idx_privchat_refresh_tokens_revoked_at
    ON privchat_refresh_tokens (revoked_at);
