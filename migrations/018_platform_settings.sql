-- 018_platform_settings.sql
-- PROFILE_VISIBILITY P2:平台级配置 k/v(spec 05-feature/PROFILE_VISIBILITY_SPEC.md D4)。
--
-- 首个用途:privacy.username_searchable —— 平台级「是否开放按用户名搜索」。
-- 邀请制/白标盘可整体关闭,个人 allow_search_by_username 只能在其之下更严
-- (deny-wins)。运行期由 admin API 读写,server 内存缓存,重启从表加载。
--
-- 通用 k/v + JSONB:后续平台级开关(qrcode/phone 搜索、名片等)直接加 key,
-- 不再出迁移。

CREATE TABLE IF NOT EXISTS privchat_platform_settings (
    key        TEXT PRIMARY KEY,
    value      JSONB  NOT NULL,
    updated_at BIGINT NOT NULL
);
