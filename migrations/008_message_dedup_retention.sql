-- P1-18: privchat_message_dedup retention。
-- 第八批起普通客户端消息的 durable idempotency 也写入本表
-- （dedup_key 形如 client:{sender_user_id}:{local_message_id}），
-- 每条消息一行、无清理则永久增长。server 内置每小时清理任务
-- （保留 7 天，远大于客户端重试窗口），本索引支撑按 created_at 的范围删除。
CREATE INDEX IF NOT EXISTS idx_message_dedup_created_at
    ON privchat_message_dedup (created_at);
