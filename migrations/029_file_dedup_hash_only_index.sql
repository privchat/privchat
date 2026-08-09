-- 029: 判重索引收敛成 hash-only
--
-- 028 最初建的是 (file_hash, file_type, file_size[, encryption_version]) 复合索引。
-- 那是多余的：SHA-256 相同即字节相同，于是大小必然相同，「明文/密文互相复用」
-- 也不成立——字节都一样了，本来就是同一份东西。三个列是同一个判据的三层同义反复。
--
-- 🔴 单独开一条 migration，而不是回去改 028：`CREATE INDEX IF NOT EXISTS` 遇到
-- 已经建好的**旧复合索引**不会替换它（名字相同就直接跳过）。已经跑过 028 的库
-- 会停在旧索引上，而代码按 hash-only 查——查得到，但走不到索引。
-- 改已执行过的 migration 只对全新库有效，对存量库是空操作。

DROP INDEX IF EXISTS idx_privchat_file_uploads_content;

CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_content
    ON privchat_file_uploads (file_hash);
