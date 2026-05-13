-- 017: Bot follow relation
-- Spec: privchat-docs/spec/02-server/SERVICE_ACCOUNT_FOLLOW_SPEC §4
--
-- 目标：承载用户与 Bot (`user_type=2`) 之间的"关注关系"。与好友体系完全独立。
--   - 同一对 (user_id, bot_user_id) UNIQUE
--   - status 切换不删行（followed -> unfollowed -> followed 复活）
--   - channel_id 由 server `direct_channel.get_or_create` 分配，后续 status 切换沿用同一 channel_id

CREATE TABLE IF NOT EXISTS privchat_bot_follow (
    id              BIGSERIAL PRIMARY KEY,
    user_id         BIGINT      NOT NULL,
    bot_user_id     BIGINT      NOT NULL,            -- 必须 user_type=2 (Bot)
    channel_id      BIGINT      NOT NULL,
    status          SMALLINT    NOT NULL DEFAULT 1,  -- 1=followed, 0=unfollowed
    followed_at     BIGINT      NOT NULL,
    unfollowed_at   BIGINT,
    created_at      BIGINT      NOT NULL DEFAULT 0,
    updated_at      BIGINT      NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_bot_follow_user_bot
    ON privchat_bot_follow (user_id, bot_user_id);

CREATE INDEX IF NOT EXISTS idx_bot_follow_bot
    ON privchat_bot_follow (bot_user_id, status);

CREATE INDEX IF NOT EXISTS idx_bot_follow_user
    ON privchat_bot_follow (user_id, status);
