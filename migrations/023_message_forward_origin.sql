-- 023: 转发来源（MEDIA_REFERENCE_AND_FORWARD_SPEC §3.3）
--
-- 单条转发产生的是**目标会话里的一条独立消息**（快照），不是指向源消息的链接。
-- 这张表只记「展示用的来源信息」，副本本身不依赖它存在。

CREATE TABLE IF NOT EXISTS privchat_message_forward_origin (
    -- 目标（转发后）消息
    message_id       BIGINT PRIMARY KEY,
    -- 最初的源消息。🔴 **绝不能配 ON DELETE CASCADE**：物理删除源消息会连带
    -- 删掉所有转发副本的来源记录，严重时删掉副本本身。这里干脆不建外键。
    root_message_id  BIGINT,
    -- 最初作者（快照，不随源消息删除而变）
    root_author_id   BIGINT NOT NULL,
    root_channel_id  BIGINT,
    -- 展示用作者名/头像快照
    display_snapshot JSONB,
    flags            INTEGER NOT NULL DEFAULT 0,
    created_at       BIGINT NOT NULL DEFAULT now_millis()
);

-- 「这条源消息被转发过多少次」用得上；也便于按作者排查滥用。
CREATE INDEX IF NOT EXISTS idx_privchat_forward_origin_root
    ON privchat_message_forward_origin (root_message_id);
