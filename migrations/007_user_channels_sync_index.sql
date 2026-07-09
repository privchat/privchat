-- P0-12: resume sync 增量化。
-- channel entity sync 的 since_version / keyset 分页已下推到 SQL
-- （WHERE user_id = $1 AND (c.sync_version > $2 OR uc.sync_version > $2)
--   ORDER BY GREATEST(...), channel_id LIMIT n）。
-- 该索引让 uc 侧的 since 过滤在用户 channel 数大时不用全行扫描。
CREATE INDEX IF NOT EXISTS idx_user_channels_user_sync
    ON privchat_user_channels (user_id, sync_version);
