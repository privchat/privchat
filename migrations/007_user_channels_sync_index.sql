-- P0-12: resume sync 增量化。
-- channel entity sync 的 since_version / keyset 分页已下推到 SQL
-- （WHERE user_id = $1 AND (c.sync_version > $2 OR uc.sync_version > $2)
--   ORDER BY GREATEST(...), channel_id LIMIT n）。
--
-- 勘误：privchat_user_channels 已有 idx_privchat_user_channels_user_sync_version
-- (user_id, sync_version DESC)，B-tree 支持反向扫描，完全覆盖该查询的 uc 侧过滤，
-- 无需新索引。本迁移收敛为清理早期误建的重复索引（幂等，无则跳过）。
DROP INDEX IF EXISTS idx_user_channels_user_sync;
