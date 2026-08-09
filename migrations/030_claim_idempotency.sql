-- 030: 秒传取用的幂等键
--
-- `file/claim_existing` 要满足的不只是「并发只有一个成功」，还得是：
-- **数据库提交了但响应丢了，客户端拿同一个 token 重试要拿回同一个 file_id**。
--
-- 🔴 这件事只能在**一个**存储里做。此前打算用 Redis SETNX，那是假原子：
--   · 先写 Redis、进程在 DB 插入前崩 → 永远显示「已取用」，却没有 file_id；
--   · 先写 DB、进程在 Redis 前崩 → 重试再插一行；
--   · DB 成功但响应丢 → 客户端仍拿不回原来的 file_id；
--   · Redis TTL 到期 → 幂等记录没了，数据库行还在。
-- 「失败时回滚那一行」也救不了进程崩溃。
--
-- 所以幂等键直接放进文件表，与那一行同事务写入：查得到就说明那次 claim 成功过。
-- 部分唯一索引限定 claim_key_hash 非空，普通上传的行不受影响。

ALTER TABLE privchat_file_uploads
    ADD COLUMN IF NOT EXISTS claim_key_hash VARCHAR(64);

CREATE UNIQUE INDEX IF NOT EXISTS uq_privchat_file_uploads_claim_key
    ON privchat_file_uploads (uploader_id, claim_key_hash)
    WHERE claim_key_hash IS NOT NULL;
