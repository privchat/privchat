-- P2-05 大表搜索优化：admin 消息搜索 `content ILIKE '%kw%'`（message_repo
-- search_messages）在 privchat_messages（RANGE 分区、最大表）上是全表顺序扫描。
-- pg_trgm 的 GIN 索引让 `ILIKE '%...%'`（子串匹配）走索引，把 seq scan 变成
-- 三元组索引扫描。分区父表上 CREATE INDEX 会传播到各分区（PG 11+）。
--
-- 生产注意：GIN 索引构建期会锁写。大库应在维护窗口执行，或改用
-- CREATE INDEX CONCURRENTLY（不能在事务内、需逐分区），本迁移用普通 CREATE
-- （migrate 命令在维护窗口跑）。
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_privchat_messages_content_trgm
    ON privchat_messages USING gin (content gin_trgm_ops);

-- admin 消息搜索还常按 sender_id 过滤 + created_at 排序（"某用户的消息"），
-- 但现有索引只有 (sender_id, pts)，不支持时间序。补一条支撑该 filter+sort。
CREATE INDEX IF NOT EXISTS idx_privchat_messages_sender_time
    ON privchat_messages (sender_id, created_at DESC);
