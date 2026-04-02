CREATE TABLE IF NOT EXISTS privchat_message_reactions (
    -- privchat_messages 是按 created_at 分区的表，父表上没有 message_id 单列唯一约束，
    -- 因此这里不能声明 message_id -> privchat_messages(message_id) 的外键。
    -- 现阶段由应用层保证 message_id 有效性，reaction 清理也走应用层逻辑。
    message_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    emoji VARCHAR(32) NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT,
    PRIMARY KEY (message_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_message_reactions_message_id
    ON privchat_message_reactions(message_id);

CREATE INDEX IF NOT EXISTS idx_message_reactions_user_id
    ON privchat_message_reactions(user_id);
