-- 为消息分区表补齐 2026-03 / 2026-04 分区，并增加 DEFAULT 分区兜底
-- 解决报错：no partition of relation "privchat_messages" found for row

-- 2026-03-01 00:00:00 UTC = 1772294400000
-- 2026-04-01 00:00:00 UTC = 1774972800000
CREATE TABLE IF NOT EXISTS privchat_messages_2026_03 PARTITION OF privchat_messages
    FOR VALUES FROM (1772294400000) TO (1774972800000);

-- 2026-04-01 00:00:00 UTC = 1774972800000
-- 2026-05-01 00:00:00 UTC = 1777564800000
CREATE TABLE IF NOT EXISTS privchat_messages_2026_04 PARTITION OF privchat_messages
    FOR VALUES FROM (1774972800000) TO (1777564800000);

-- 兜底分区：即使未来月份分区漏建，也不会因找不到分区而写入失败
CREATE TABLE IF NOT EXISTS privchat_messages_default PARTITION OF privchat_messages DEFAULT;
