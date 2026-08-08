-- 025: 黑名单补 reason 列（BLACKLIST_SPEC）
--
-- 背景：`BlacklistService` 一直只用进程内 `HashMap`，`privchat_blacklist` 表建了
-- 但从未被读写。后果是**拉黑关系在服务重启后立即消失**，多实例下也各说各话——
-- 而拉黑是「对方不能再发消息给我」这类用户明确预期长期生效的设置。
--
-- 服务改为以 DB 为真源，实体里已有的 reason 字段需要一个落脚的列。

ALTER TABLE privchat_blacklist
    ADD COLUMN IF NOT EXISTS reason VARCHAR(256);
