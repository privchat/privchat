-- 026: 群「仅群主/管理员可发言」落库
--
-- `allow_member_post` 之前只活在 `ChannelSettings` 这份**内存**缓存里，而且没有任何
-- 写入入口——发送闸口读得到它，却没人能把它改成 false，也没有任何东西能让它跨重启存活。
-- 也就是说这条策略在生产里是不可达配置：读它的代码存在，值永远是默认的 true。
--
-- 和 all_muted / forbid_forward 一样放进 privchat_groups：DB 是真源，重启后仍生效。
--
-- 语义（与 all_muted 正交，任一为限制态即拒绝发送）：
--   all_muted          = 临时把所有人闭麦
--   allow_member_post  = 本群常态只读（公告群）
-- 群主与管理员两者都不受限制。

ALTER TABLE privchat_groups
    ADD COLUMN IF NOT EXISTS allow_member_post BOOLEAN NOT NULL DEFAULT true;
