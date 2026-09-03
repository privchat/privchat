-- P2-05 大表搜索优化：admin 消息搜索 `content ILIKE '%kw%'`（message_repo
-- search_messages）在 privchat_messages（RANGE 分区、最大表）上是全表顺序扫描。
-- pg_trgm 的 GIN 索引让 `ILIKE '%...%'`（子串匹配）走索引，把 seq scan 变成
-- 三元组索引扫描。分区父表上 CREATE INDEX 会传播到各分区（PG 11+）。
--
-- 生产注意：GIN 索引构建期会锁写。大库应在维护窗口执行，或改用
-- CREATE INDEX CONCURRENTLY（不能在事务内、需逐分区），本迁移用普通 CREATE
-- （migrate 命令在维护窗口跑）。
-- 🔴 pg_trgm 属于 contrib，托管 PostgreSQL 常常没装（生产库 pg_available_extensions
-- 里只有 plpgsql）。硬写 CREATE EXTENSION 会让整条迁移链在这一步断掉，而这条索引
-- 只是 admin 后台子串搜索的**性能优化**，缺了不影响正确性——客户端搜索走的是 011
-- 的 bigram 索引，不依赖任何扩展。
--
-- 所以按可用性降级，并且**明确告警**：不装作没事发生，让人知道这台库上少了什么。
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'pg_trgm') THEN
        CREATE EXTENSION IF NOT EXISTS pg_trgm;
        CREATE INDEX IF NOT EXISTS idx_privchat_messages_content_trgm
            ON privchat_messages USING gin (content gin_trgm_ops);
    ELSE
        RAISE WARNING '这台库没有 pg_trgm，跳过 admin 子串搜索的 GIN 索引：admin 消息搜索会退化为顺序扫描。客户端搜索不受影响（走 011 的 bigram 索引）。要恢复请在数据库主机安装 postgresql-contrib 后重建此索引。';
    END IF;
END
$$;

-- admin 消息搜索还常按 sender_id 过滤 + created_at 排序（"某用户的消息"），
-- 但现有索引只有 (sender_id, pts)，不支持时间序。补一条支撑该 filter+sort。
CREATE INDEX IF NOT EXISTS idx_privchat_messages_sender_time
    ON privchat_messages (sender_id, created_at DESC);
